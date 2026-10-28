//! Admin monitor, broker metrics, user management, Ollama controls, log viewer, and database explorer built with Iced 0.14.
//!
//! Provides Elm-architecture messages ([`AdminMessage`]), Iced view components ([`admin_dialog_view`]),
//! state management ([`AdminViewState`], [`AdminState`]), and 2-second telemetry subscription ([`admin_subscription`]).

#![allow(dead_code)]

use chatty_protocol::{
    AdapterStatus, AdminDataRow, BrokerConfig, BrokerLog, BrokerMonitor, OllamaAction, OllamaState,
    Request, Role, UserAccount,
};
use iced::widget::{
    Space, button, column, container, pick_list, row, rule, scrollable, text, text_input, toggler,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Theme};

use crate::ui;
use std::time::{Duration, Instant};

// -----------------------------------------------------------------------------
// Admin Messages
// -----------------------------------------------------------------------------

/// User interaction messages for the Admin portal dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum AdminMessage {
    // Tab switching
    SelectTab(usize),

    // Broker monitor & reboot
    RefreshMonitor,
    SoftReboot,
    SaveBrokerConfig,

    // Broker configuration form controls
    ToggleAdapter(bool),
    AdapterUrlChanged(String),
    SelectRuntimeAdapter(String),
    SelectAdapterModel(String),
    TemperatureChanged(f32),
    TopPChanged(f32),
    TopKChanged(u32),
    NumCtxChanged(u32),
    NumPredictChanged(i32),
    RepeatPenaltyChanged(f32),
    SeedChanged(i64),
    KeepAliveChanged(String),
    TogglePublicCharacters(bool),
    ToggleRegistration(bool),

    // Image generation settings
    ToggleImages(bool),
    ImageUrlChanged(String),
    SelectImagePlugin(String),
    ImageModelChanged(String),
    ImageWidthChanged(u32),
    ImageHeightChanged(u32),
    ImageStepsChanged(u32),
    ImageGuidanceChanged(f32),
    ImageNegativeChanged(String),
    LoadImageModels,
    LoadImageWorkflow,
    ClearImageWorkflow,
    ImageTestPromptChanged(String),
    TestImageGeneration,

    ModelsPage(usize),
    ToggleAdvancedSettings(bool),
    ModelSearchChanged(String),
    SearchModels,
    DownloadModel(String),
    CancelDownload(String),
    ClearFinishedDownloads,
    ChooseModelFile {
        id: String,
        file: String,
    },
    InstalledSearchChanged(String),
    ConfirmModelDelete(String),
    CancelModelDelete,
    ConfigureModel(String),
    // Ollama actions
    RefreshOllama,
    OllamaPullModelChanged(String),
    OllamaPull,
    OllamaLoad(String),
    OllamaUnload(String),
    OllamaDelete(String),

    // User management
    NewUsernameChanged(String),
    NewPasswordChanged(String),
    NewRoleChanged(Role),
    CreateUser,
    SetUserRole {
        user_id: String,
        role: Role,
    },
    DeleteUser(String),

    // Logs & Data
    RefreshLogs,
    SearchLogsChanged(String),
    CopyLogs,
    RefreshData,
    ToggleCharacterPublic {
        character_id: String,
        is_public: bool,
    },

    // Dialog dismissal
    CloseDialog,
}

impl AdminMessage {
    #[must_use]
    pub fn set_user_role(user_id: impl Into<String>, role: Role) -> Self {
        Self::SetUserRole {
            user_id: user_id.into(),
            role,
        }
    }

    #[must_use]
    pub fn toggle_character_public(character_id: impl Into<String>, is_public: bool) -> Self {
        Self::ToggleCharacterPublic {
            character_id: character_id.into(),
            is_public,
        }
    }
}

// -----------------------------------------------------------------------------
// Admin View State & Standalone State
// -----------------------------------------------------------------------------

/// Displayable role options for pick lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminRoleOption {
    User,
    Admin,
}

impl std::fmt::Display for AdminRoleOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "User"),
            Self::Admin => write!(f, "Admin"),
        }
    }
}

impl From<Role> for AdminRoleOption {
    fn from(r: Role) -> Self {
        match r {
            Role::Admin => Self::Admin,
            Role::User => Self::User,
        }
    }
}

impl From<AdminRoleOption> for Role {
    fn from(o: AdminRoleOption) -> Self {
        match o {
            AdminRoleOption::Admin => Role::Admin,
            AdminRoleOption::User => Role::User,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelsState {
    pub downloads: std::collections::BTreeMap<String, ModelDownload>,
    pub saved_adapter_url: Option<String>,
    pub saved_model: String,
    pub page: usize,
    pub advanced_settings: bool,
    pub query: String,
    pub installed_query: String,
    pub results: Vec<chatty_protocol::ModelSearchResult>,
    pub selected_files: std::collections::HashMap<String, String>,
    pub delete_model: Option<String>,
    pub busy: bool,
    pub request_id: Option<u64>,
    pub notice: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ModelDownload {
    pub destination: String,
    pub request_id: Option<u64>,
    pub progress: chatty_protocol::admin::ModelDownloadProgress,
    pub finished: bool,
    pub cancelling: bool,
}

impl ModelsState {
    pub fn is_downloading(&self, model: &str) -> bool {
        self.downloads
            .get(model)
            .is_some_and(|download| !download.finished)
    }
    pub fn can_download(&self, model: &str) -> bool {
        !model.trim().is_empty()
            && !self.is_downloading(model.trim())
            && self
                .downloads
                .values()
                .filter(|download| !download.finished)
                .count()
                < chatty_protocol::admin::MAX_PARALLEL_DOWNLOADS
    }
    fn queue_download(&mut self, model: String, destination: String) {
        if self.downloads.len() >= 16 {
            self.downloads.retain(|_, download| !download.finished);
        }
        self.downloads.insert(
            model,
            ModelDownload {
                destination,
                progress: chatty_protocol::admin::ModelDownloadProgress {
                    status: "Starting download…".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
    }
    /// Return true if a cancellation was requested before the request ID arrived.
    pub fn download_started(&mut self, model: &str, id: u64) -> bool {
        let Some(download) = self
            .downloads
            .get_mut(model)
            .filter(|download| !download.finished)
        else {
            return true;
        };
        download.request_id = Some(id);
        download.cancelling
    }
    pub fn download_for_request(&mut self, id: u64) -> Option<&mut ModelDownload> {
        self.downloads
            .values_mut()
            .find(|download| !download.finished && download.request_id == Some(id))
    }
    pub fn cancel_download(&mut self, model: &str) -> Option<u64> {
        let download = self
            .downloads
            .get_mut(model)
            .filter(|download| !download.finished && !download.cancelling)?;
        download.cancelling = true;
        download.request_id
    }
    pub fn interrupt_downloads(&mut self) {
        for download in self
            .downloads
            .values_mut()
            .filter(|download| !download.finished)
        {
            download.finished = true;
            download.request_id = None;
            download.progress.status =
                "Connection interrupted. Download stopped; retry to resume available partial data."
                    .into();
        }
    }
}

/// Read-only snapshot of admin portal state passed to Iced view functions.
#[derive(Debug, Clone)]
pub struct AdminViewState<'a> {
    pub models: &'a ModelsState,
    pub broker_config: &'a BrokerConfig,
    pub adapter_plugins: &'a [String],
    pub adapter_override: Option<&'a str>,
    pub image_plugins: &'a [String],
    pub image_models: Option<&'a [String]>,
    pub image_workflow_summary: &'a str,
    pub image_notice: Option<&'a str>,
    pub image_test: &'a ImageTest,
    pub broker_monitor: Option<&'a BrokerMonitor>,
    pub ollama_state: Option<&'a OllamaState>,
    pub ollama_pull_model: &'a str,
    pub admin_new_username: &'a str,
    pub admin_new_password: &'a str,
    pub admin_new_role: Role,
    pub users: &'a [UserAccount],
    pub current_user_id: &'a str,
    pub admin_log: &'a BrokerLog,
    pub admin_log_search: &'a str,
    pub admin_data: &'a [AdminDataRow],
    pub admin_tab: usize,
    pub glass_mode: bool,
}

/// Owned admin state suitable for standalone usage, caching, and unit testing.
#[derive(Debug, Clone)]
pub struct AdminState {
    pub models: ModelsState,
    pub broker_config: BrokerConfig,
    pub adapter_plugins: Vec<String>,
    pub adapter_override: Option<String>,
    pub image_plugins: Vec<String>,
    /// Models listed by the image service on explicit request; `None` until listed.
    pub image_models: Option<Vec<String>>,
    /// Cached description of `broker_config.image.workflow`, rebuilt on change.
    pub image_workflow_summary: String,
    pub image_notice: Option<String>,
    pub image_test: ImageTest,
    pub broker_monitor: Option<BrokerMonitor>,
    pub ollama_state: Option<OllamaState>,
    pub ollama_pull_model: String,
    pub admin_new_username: String,
    pub admin_new_password: String,
    pub admin_new_role: Role,
    pub users: Vec<UserAccount>,
    pub current_user_id: String,
    pub admin_log: BrokerLog,
    pub admin_log_search: String,
    pub admin_data: Vec<AdminDataRow>,
    pub admin_tab: usize,
    pub glass_mode: bool,
}

impl Default for AdminState {
    fn default() -> Self {
        Self {
            models: ModelsState::default(),
            adapter_plugins: Vec::new(),
            adapter_override: None,
            image_plugins: Vec::new(),
            image_models: None,
            image_workflow_summary: workflow_summary(""),
            image_notice: None,
            image_test: ImageTest::default(),
            broker_config: BrokerConfig {
                adapter_enabled: false,
                adapter_url: String::new(),
                adapter_plugin: String::new(),
                use_ollama_api: false,
                model: String::new(),
                temperature: 0.8,
                top_p: 0.9,
                top_k: 40,
                num_ctx: 4096,
                num_predict: -1,
                repeat_penalty: 1.1,
                seed: -1,
                keep_alive: "5m".into(),
                allow_public_characters: false,
                allow_self_registration: true,
                image: Default::default(),
            },
            broker_monitor: None,
            ollama_state: None,
            ollama_pull_model: String::new(),
            admin_new_username: String::new(),
            admin_new_password: String::new(),
            admin_new_role: Role::User,
            users: Vec::new(),
            current_user_id: String::new(),
            admin_log: BrokerLog::default(),
            admin_log_search: String::new(),
            admin_data: Vec::new(),
            admin_tab: 0,
            glass_mode: false,
        }
    }
}

impl AdminState {
    #[must_use]
    pub fn as_view_state(&self) -> AdminViewState<'_> {
        AdminViewState {
            models: &self.models,
            broker_config: &self.broker_config,
            adapter_plugins: &self.adapter_plugins,
            adapter_override: self.adapter_override.as_deref(),
            image_plugins: &self.image_plugins,
            image_models: self.image_models.as_deref(),
            image_workflow_summary: &self.image_workflow_summary,
            image_notice: self.image_notice.as_deref(),
            image_test: &self.image_test,
            broker_monitor: self.broker_monitor.as_ref(),
            ollama_state: self.ollama_state.as_ref(),
            ollama_pull_model: &self.ollama_pull_model,
            admin_new_username: &self.admin_new_username,
            admin_new_password: &self.admin_new_password,
            admin_new_role: self.admin_new_role,
            users: &self.users,
            current_user_id: &self.current_user_id,
            admin_log: &self.admin_log,
            admin_log_search: &self.admin_log_search,
            admin_data: &self.admin_data,
            admin_tab: self.admin_tab,
            glass_mode: self.glass_mode,
        }
    }

    /// Handles an [`AdminMessage`], updating local state and returning any protocol [`Request`]s.
    pub fn update(&mut self, message: AdminMessage, session_token: &str) -> Vec<Request> {
        if matches!(
            &message,
            AdminMessage::OllamaPull | AdminMessage::DownloadModel(_)
        ) && self
            .models
            .saved_adapter_url
            .as_ref()
            .is_some_and(|url| url != &self.broker_config.adapter_url)
        {
            self.models.notice = Some("Save server changes in Settings.".into());
            return vec![];
        }
        if let AdminMessage::DownloadModel(model) = message {
            self.ollama_pull_model = model;
            return self.update(AdminMessage::OllamaPull, session_token);
        }
        if matches!(&message, AdminMessage::OllamaPull) {
            let model = self.ollama_pull_model.trim().to_owned();
            if !self.models.can_download(&model) {
                return vec![];
            }
            self.models
                .queue_download(model.clone(), self.broker_config.adapter_url.clone());
            return vec![Request::AdminOllamaAction {
                session_token: session_token.into(),
                action: OllamaAction::Pull { model },
            }];
        }
        let model_action = matches!(
            &message,
            AdminMessage::OllamaLoad(_)
                | AdminMessage::OllamaUnload(_)
                | AdminMessage::OllamaDelete(_)
                | AdminMessage::SearchModels
                | AdminMessage::RefreshOllama
        );
        if model_action && self.models.busy {
            return vec![];
        }
        if let AdminMessage::OllamaDelete(model) = &message {
            let running = self
                .ollama_state
                .as_ref()
                .is_some_and(|s| s.running_models.iter().any(|m| &m.name == model));
            if self.models.delete_model.as_ref() != Some(model)
                || running
                || self.broker_config.model == *model
                || self.models.saved_model == *model
                || self.models.is_downloading(model)
            {
                return vec![];
            }
            self.models.delete_model = None;
        }
        if model_action {
            self.models.busy = true;
            self.models.notice = Some(match &message {
                AdminMessage::SearchModels => "Searching Hugging Face…".into(),
                _ => "Updating models…".into(),
            });
        }
        match message {
            AdminMessage::DownloadModel(_) => unreachable!(),
            AdminMessage::CancelDownload(model) => {
                self.models.cancel_download(&model);
                vec![]
            }
            AdminMessage::ClearFinishedDownloads => {
                self.models
                    .downloads
                    .retain(|_, download| !download.finished);
                vec![]
            }
            AdminMessage::ModelsPage(page) => {
                self.admin_tab = 2;
                self.models.page = page;
                vec![]
            }
            AdminMessage::ToggleAdvancedSettings(expanded) => {
                self.models.advanced_settings = expanded;
                vec![]
            }
            AdminMessage::ModelSearchChanged(query) => {
                self.models.query = query;
                vec![]
            }
            AdminMessage::InstalledSearchChanged(query) => {
                self.models.installed_query = query;
                vec![]
            }
            AdminMessage::ChooseModelFile { id, file } => {
                self.ollama_pull_model = format!("hf.co/{id}:{file}");
                self.models.selected_files.insert(id, file);
                vec![]
            }
            AdminMessage::SearchModels => vec![Request::AdminSearchModels {
                session_token: session_token.into(),
                query: self.models.query.trim().into(),
            }],
            AdminMessage::ConfirmModelDelete(model) => {
                self.models.delete_model = Some(model);
                vec![]
            }
            AdminMessage::CancelModelDelete => {
                self.models.delete_model = None;
                vec![]
            }
            AdminMessage::ConfigureModel(model) => {
                self.broker_config.model = model;
                self.models.page = 2;
                vec![]
            }

            AdminMessage::SelectTab(tab) => {
                self.admin_tab = tab;
                if tab == 2 && self.models.busy {
                    return vec![];
                }
                requests_for_tab(tab, session_token)
            }
            AdminMessage::RefreshMonitor => {
                vec![Request::AdminGetBrokerMonitor {
                    session_token: session_token.to_string(),
                }]
            }
            AdminMessage::SoftReboot => {
                vec![Request::AdminSoftReboot {
                    session_token: session_token.to_string(),
                }]
            }
            AdminMessage::SaveBrokerConfig => {
                vec![Request::AdminSetBrokerConfig {
                    session_token: session_token.to_string(),
                    config: self.broker_config.clone(),
                }]
            }
            AdminMessage::ToggleAdapter(enabled) => {
                self.broker_config.adapter_enabled = enabled;
                vec![]
            }
            AdminMessage::AdapterUrlChanged(url) => {
                self.broker_config.adapter_url = url;
                vec![]
            }
            AdminMessage::SelectRuntimeAdapter(id) => {
                if self.adapter_override.is_none() && self.adapter_plugins.contains(&id) {
                    self.broker_config.use_ollama_api = id == "ollama";
                    self.broker_config.adapter_plugin = id;
                }
                vec![]
            }
            AdminMessage::SelectAdapterModel(model) => {
                self.broker_config.model = model;
                vec![]
            }
            AdminMessage::TemperatureChanged(temp) => {
                self.broker_config.temperature = temp;
                vec![]
            }
            AdminMessage::TopPChanged(top_p) => {
                self.broker_config.top_p = top_p;
                vec![]
            }
            AdminMessage::TopKChanged(top_k) => {
                self.broker_config.top_k = top_k;
                vec![]
            }
            AdminMessage::NumCtxChanged(ctx) => {
                self.broker_config.num_ctx = ctx;
                vec![]
            }
            AdminMessage::NumPredictChanged(predict) => {
                self.broker_config.num_predict = predict;
                vec![]
            }
            AdminMessage::RepeatPenaltyChanged(penalty) => {
                self.broker_config.repeat_penalty = penalty;
                vec![]
            }
            AdminMessage::SeedChanged(seed) => {
                self.broker_config.seed = seed;
                vec![]
            }
            AdminMessage::KeepAliveChanged(dur) => {
                self.broker_config.keep_alive = dur;
                vec![]
            }
            AdminMessage::TogglePublicCharacters(allow) => {
                self.broker_config.allow_public_characters = allow;
                vec![]
            }
            AdminMessage::ToggleRegistration(allow) => {
                self.broker_config.allow_self_registration = allow;
                vec![]
            }
            AdminMessage::ToggleImages(enabled) => {
                self.broker_config.image.enabled = enabled;
                vec![]
            }
            AdminMessage::ImageUrlChanged(url) => {
                self.image_models = None;
                self.broker_config.image.url = url;
                vec![]
            }
            AdminMessage::SelectImagePlugin(id) => {
                if self.image_plugins.contains(&id) {
                    self.image_models = None;
                    self.broker_config.image.plugin = id;
                }
                vec![]
            }
            AdminMessage::ImageModelChanged(model) => {
                self.broker_config.image.model = model;
                vec![]
            }
            AdminMessage::ImageWidthChanged(width) => {
                self.broker_config.image.width = width;
                vec![]
            }
            AdminMessage::ImageHeightChanged(height) => {
                self.broker_config.image.height = height;
                vec![]
            }
            AdminMessage::ImageStepsChanged(steps) => {
                self.broker_config.image.steps = steps;
                vec![]
            }
            AdminMessage::ImageGuidanceChanged(guidance) => {
                self.broker_config.image.guidance = guidance;
                vec![]
            }
            AdminMessage::ImageNegativeChanged(prompt) => {
                self.broker_config.image.negative_prompt = prompt;
                vec![]
            }
            AdminMessage::LoadImageModels => {
                vec![Request::AdminListImageModels {
                    session_token: session_token.to_string(),
                }]
            }
            AdminMessage::LoadImageWorkflow => {
                match pick_image_workflow() {
                    Some(Ok(workflow)) => {
                        self.image_workflow_summary = workflow_summary(&workflow);
                        self.broker_config.image.workflow = workflow;
                        self.image_notice =
                            Some("Workflow loaded. Save image settings to use it.".into());
                    }
                    Some(Err(error)) => self.image_notice = Some(error),
                    None => {}
                }
                vec![]
            }
            AdminMessage::ClearImageWorkflow => {
                self.broker_config.image.workflow.clear();
                self.image_workflow_summary = workflow_summary("");
                self.image_notice =
                    Some("Built-in workflow selected. Save image settings to use it.".into());
                vec![]
            }
            AdminMessage::ImageTestPromptChanged(prompt) => {
                self.image_test.prompt = prompt;
                vec![]
            }
            AdminMessage::TestImageGeneration => {
                if self.image_test.pending {
                    return vec![];
                }
                let prompt = match self.image_test.prompt.trim() {
                    "" => DEFAULT_TEST_PROMPT.to_owned(),
                    prompt => prompt.to_owned(),
                };
                self.image_test = ImageTest {
                    prompt: self.image_test.prompt.clone(),
                    pending: true,
                    started: Some(Instant::now()),
                    ..ImageTest::default()
                };
                vec![Request::GenerateImage {
                    session_token: session_token.to_string(),
                    prompt,
                    negative_prompt: String::new(),
                    square: false,
                    seed: None,
                }]
            }
            AdminMessage::RefreshOllama => {
                vec![Request::AdminGetOllamaState {
                    session_token: session_token.to_string(),
                }]
            }
            AdminMessage::OllamaPullModelChanged(model) => {
                self.ollama_pull_model = model;
                vec![]
            }
            AdminMessage::OllamaPull => unreachable!(),
            AdminMessage::OllamaLoad(model) => {
                vec![Request::AdminOllamaAction {
                    session_token: session_token.to_string(),
                    action: OllamaAction::Load { model },
                }]
            }
            AdminMessage::OllamaUnload(model) => {
                vec![Request::AdminOllamaAction {
                    session_token: session_token.to_string(),
                    action: OllamaAction::Unload { model },
                }]
            }
            AdminMessage::OllamaDelete(model) => {
                vec![Request::AdminOllamaAction {
                    session_token: session_token.to_string(),
                    action: OllamaAction::Delete { model },
                }]
            }
            AdminMessage::NewUsernameChanged(u) => {
                self.admin_new_username = u;
                vec![]
            }
            AdminMessage::NewPasswordChanged(p) => {
                self.admin_new_password = p;
                vec![]
            }
            AdminMessage::NewRoleChanged(r) => {
                self.admin_new_role = r;
                vec![]
            }
            AdminMessage::CreateUser => {
                let username = self.admin_new_username.trim().to_string();
                let password = self.admin_new_password.clone();
                let role = self.admin_new_role;
                self.admin_new_password.clear();
                vec![Request::AdminCreateUser {
                    session_token: session_token.to_string(),
                    username,
                    password,
                    role,
                }]
            }
            AdminMessage::SetUserRole { user_id, role } => {
                vec![Request::AdminSetRole {
                    session_token: session_token.to_string(),
                    user_id,
                    role,
                }]
            }
            AdminMessage::DeleteUser(user_id) => {
                vec![Request::AdminDeleteUser {
                    session_token: session_token.to_string(),
                    user_id,
                }]
            }
            AdminMessage::RefreshLogs => {
                vec![Request::AdminReadBrokerLog {
                    session_token: session_token.to_string(),
                }]
            }
            AdminMessage::SearchLogsChanged(s) => {
                self.admin_log_search = s;
                vec![]
            }
            AdminMessage::CopyLogs => vec![],
            AdminMessage::RefreshData => {
                vec![Request::AdminReadDatabase {
                    session_token: session_token.to_string(),
                }]
            }
            AdminMessage::ToggleCharacterPublic {
                character_id,
                is_public,
            } => {
                vec![Request::AdminSetCharacterPublic {
                    session_token: session_token.to_string(),
                    character_id,
                    is_public,
                }]
            }
            AdminMessage::CloseDialog => vec![],
        }
    }
}

// -----------------------------------------------------------------------------
// Protocol & Formatting Helpers
// -----------------------------------------------------------------------------

/// Constructs the protocol requests needed when navigating to an admin tab.
#[must_use]
pub fn requests_for_tab(tab: usize, session_token: &str) -> Vec<Request> {
    match tab {
        0 => vec![
            Request::AdminGetBrokerConfig {
                session_token: session_token.to_string(),
            },
            Request::AdminGetBrokerMonitor {
                session_token: session_token.to_string(),
            },
        ],
        1 => vec![Request::AdminListUsers {
            session_token: session_token.to_string(),
        }],
        2 => vec![
            Request::AdminGetOllamaState {
                session_token: session_token.to_string(),
            },
            Request::AdminGetBrokerConfig {
                session_token: session_token.to_string(),
            },
        ],
        3 => vec![Request::AdminGetBrokerConfig {
            session_token: session_token.to_string(),
        }],
        4 => vec![Request::AdminReadBrokerLog {
            session_token: session_token.to_string(),
        }],
        _ => vec![Request::AdminReadDatabase {
            session_token: session_token.to_string(),
        }],
    }
}

/// Formats seconds into human-readable duration (`Xd Xh Xm` or `Xh Xm Xs` or `Xs`).
#[must_use]
pub fn format_duration(s: u64) -> String {
    let days = s / 86400;
    let hours = (s % 86400) / 3600;
    let mins = (s % 3600) / 60;
    let secs = s % 60;
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m {secs}s")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

/// Formats byte quantities into KiB, MiB, or GiB.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Formats an integer with comma separators for readable token display.
#[must_use]
pub fn format_token_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// -----------------------------------------------------------------------------
// Telemetry Subscription
// -----------------------------------------------------------------------------

/// Telemetry subscription that emits `AdminMessage::RefreshMonitor` every 2 seconds
/// ONLY when the Broker tab is active and visible (`active == true`).
/// Produces `Subscription::none()` with zero wakeups or timers when inactive.
pub fn admin_subscription(active: bool) -> iced::Subscription<AdminMessage> {
    if active {
        iced::time::every(std::time::Duration::from_secs(2)).map(|_| AdminMessage::RefreshMonitor)
    } else {
        iced::Subscription::none()
    }
}

/// Helper subscription checking both whether admin is open and whether Broker tab (tab 0) is active.
pub fn admin_subscription_gated(
    is_admin_open: bool,
    active_tab: usize,
) -> iced::Subscription<AdminMessage> {
    admin_subscription(is_admin_open && active_tab == 0)
}

// -----------------------------------------------------------------------------
// Iced 0.14 View Components
// -----------------------------------------------------------------------------

/// Prompt used by the Images tab test when its prompt box is empty.
pub const DEFAULT_TEST_PROMPT: &str = "a lighthouse on a cliff at dusk, dramatic sky";

/// One administrator test render using the broker's saved image settings.
#[derive(Debug, Clone, Default)]
pub struct ImageTest {
    pub prompt: String,
    pub pending: bool,
    pub request_id: Option<u64>,
    pub started: Option<Instant>,
    pub result: Option<Vec<u8>>,
    pub elapsed: Option<Duration>,
    pub notice: Option<String>,
}

impl AdminState {
    /// Apply a broker configuration and refresh the derived workflow summary.
    pub fn set_broker_config(&mut self, config: BrokerConfig) {
        self.image_workflow_summary = workflow_summary(&config.image.workflow);
        self.broker_config = config;
    }
}

/// Describe a workflow template without keeping parsed JSON in view state.
#[must_use]
pub fn workflow_summary(workflow: &str) -> String {
    if workflow.is_empty() {
        return "Built-in checkpoint workflow (checkpoint, prompts, KSampler, decode).".into();
    }
    let nodes = serde_json::from_str::<serde_json::Value>(workflow)
        .ok()
        .and_then(|graph| graph.as_object().map(|nodes| nodes.len()))
        .unwrap_or(0);
    let used: Vec<&str> = chatty_protocol::admin::IMAGE_WORKFLOW_PLACEHOLDERS
        .into_iter()
        .filter(|placeholder| workflow.contains(placeholder))
        .collect();
    format!(
        "Custom workflow: {nodes} nodes, {} KiB. Fills {}.",
        workflow.len().div_ceil(1024),
        used.join(", ")
    )
}

/// Check the shape the broker requires, for early feedback before saving.
pub fn check_image_workflow(workflow: &str) -> Result<(), String> {
    if workflow.len() > chatty_protocol::admin::MAX_IMAGE_WORKFLOW_BYTES {
        return Err("The workflow file is larger than 256 KiB.".into());
    }
    let graph: serde_json::Value = serde_json::from_str(workflow)
        .map_err(|error| format!("The workflow is not valid JSON: {error}"))?;
    let nodes = graph
        .as_object()
        .ok_or("The workflow must be a JSON object of nodes.")?;
    if nodes.contains_key("nodes") && nodes.contains_key("links") {
        return Err(
            "This is a ComfyUI UI workflow. In ComfyUI use Workflow > Export (API) instead.".into(),
        );
    }
    if !workflow.contains("{{prompt}}") {
        return Err("Put {{prompt}} in the positive prompt text of the workflow.".into());
    }
    Ok(())
}

/// Ask for an API-format workflow file. `None` means the dialog was cancelled.
fn pick_image_workflow() -> Option<Result<String, String>> {
    let path = rfd::FileDialog::new()
        .add_filter("ComfyUI API workflow", &["json"])
        .pick_file()?;
    Some((|| {
        let bytes =
            std::fs::read(&path).map_err(|error| format!("Could not read the file: {error}"))?;
        let workflow = String::from_utf8(bytes)
            .map_err(|_| "The workflow file is not UTF-8 text.".to_owned())?;
        check_image_workflow(&workflow)?;
        Ok(workflow)
    })())
}

pub const ADMIN_TAB_NAMES: [&str; 6] = ["Broker", "Users", "Models", "Images", "Logs", "Data"];

/// Renders the complete Admin modal dialog including tab bar and active tab content.
pub fn admin_dialog_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    // Modal header
    let header = row![
        text("Admin Portal").size(18),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding(Padding::from([2, 8]))
            .on_press(Message::from(AdminMessage::CloseDialog)),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill);

    // Tab navigation buttons
    let mut tab_row = row![].spacing(6).align_y(Alignment::Center);
    for (idx, name) in ADMIN_TAB_NAMES.iter().enumerate() {
        let is_selected = state.admin_tab == idx;
        let tab_btn = button(text(*name).size(13).color(if is_selected {
            ui::COLOR_ON_PRIMARY
        } else {
            ui::SECONDARY_TEXT
        }))
        .padding(Padding::from([6, 14]))
        .style(move |theme: &Theme, status: button::Status| {
            let (bg, border_color) = if is_selected {
                (ui::COLOR_PRIMARY_STRONG, ui::COLOR_PRIMARY_STRONG)
            } else {
                match status {
                    button::Status::Hovered => (
                        Color::from_rgba(1.0, 1.0, 1.0, 0.12),
                        Color::from_rgba(1.0, 1.0, 1.0, 0.22),
                    ),
                    button::Status::Pressed => (
                        Color::from_rgba(1.0, 1.0, 1.0, 0.18),
                        Color::from_rgba(1.0, 1.0, 1.0, 0.30),
                    ),
                    _ => (
                        Color::from_rgba(1.0, 1.0, 1.0, 0.05),
                        Color::from_rgba(1.0, 1.0, 1.0, 0.10),
                    ),
                }
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: if is_selected {
                    ui::COLOR_ON_PRIMARY
                } else {
                    theme.palette().text
                },
                border: Border {
                    color: border_color,
                    width: 0.0,
                    radius: 6.0.into(),
                },
                shadow: iced::Shadow::default(),
                snap: false,
            }
        })
        .on_press(Message::from(AdminMessage::SelectTab(idx)));

        tab_row = tab_row.push(tab_btn);
    }

    // Active tab body
    let tab_content: Element<'a, Message> = match state.admin_tab {
        0 => broker_tab_view(state),
        1 => users_tab_view(state),
        2 => ollama_tab_view(state),
        3 => image_settings::view(state),
        4 => logs_tab_view(state),
        _ => data_tab_view(state),
    };

    let scroll = scrollable(tab_content)
        .width(Length::Fill)
        .height(Length::Fill);

    let content_col = column![header, tab_row.wrap(), rule::horizontal(1), scroll]
        .spacing(12)
        .width(Length::Fill)
        .height(Length::Fill);

    container(content_col)
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Helper creating a standardized metric display card.
fn metric_card<'a, Message: 'a>(
    label: &'static str,
    val: String,
    glass_mode: bool,
) -> Element<'a, Message> {
    let content = column![
        text(label).size(12).color(ui::SECONDARY_TEXT),
        text(val).size(15),
    ]
    .spacing(3);

    container(content)
        .padding(Padding::from([8, 12]))
        .style(move |_| ui::card_style(glass_mode))
        .into()
}

/// Broker Tab: broker metrics display, recent errors, adapter & generation parameter settings.
pub fn broker_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![].spacing(14).width(Length::Fill);

    // 1. Monitoring Section
    items = items.push(text("Monitoring").size(16));

    if let Some(m) = state.broker_monitor {
        let mem_str = match m.memory_limit_mb {
            Some(limit) => format!("{} / {} MB", m.memory_used_mb, limit),
            None => format!("{} MB", m.memory_used_mb),
        };
        let adapter_str = match m.adapter_status {
            AdapterStatus::Disabled => "Disabled".to_string(),
            AdapterStatus::Online => format!(
                "Online · {} models · {} ms",
                m.adapter_model_count,
                m.adapter_latency_ms.unwrap_or_default()
            ),
            AdapterStatus::Offline => "Offline".to_string(),
        };

        let metrics_row = row![
            metric_card(
                "Uptime",
                format_duration(m.uptime_seconds),
                state.glass_mode
            ),
            metric_card("CPU", format!("{:.1}%", m.cpu_percent), state.glass_mode),
            metric_card("Memory", mem_str, state.glass_mode),
            metric_card(
                "Connections",
                m.active_connections.to_string(),
                state.glass_mode
            ),
            metric_card("Adapter", adapter_str, state.glass_mode),
        ]
        .spacing(8);

        items = items.push(metrics_row.wrap());

        if !m.recent_errors.is_empty() {
            let mut err_col = column![text("Recent errors:").size(13)].spacing(4);
            for err in &m.recent_errors {
                err_col = err_col.push(
                    text(format!("• {err}"))
                        .size(12)
                        .color(Color::from_rgb(0.95, 0.3, 0.3)),
                );
            }
            items = items.push(err_col);
        }
    } else {
        items = items.push(
            text("Monitoring data has not loaded.")
                .size(13)
                .color(Color::from_rgba(0.6, 0.6, 0.6, 1.0)),
        );
    }

    let monitor_actions = row![
        button(text("Refresh")).on_press(Message::from(AdminMessage::RefreshMonitor)),
        button(text("Soft Reboot")).on_press(Message::from(AdminMessage::SoftReboot)),
    ]
    .spacing(8);
    items = items.push(monitor_actions.wrap());

    items = items.push(rule::horizontal(1));

    items = items
        .push(button(text("Model settings")).on_press(Message::from(AdminMessage::ModelsPage(2))));

    // 4. Policy Section
    items = items.push(text("Policy").size(16));

    let public_chars = toggler(state.broker_config.allow_public_characters)
        .label("Allow public characters")
        .on_toggle(|v| Message::from(AdminMessage::TogglePublicCharacters(v)));

    let registration = toggler(state.broker_config.allow_self_registration)
        .label("Allow self-registration")
        .on_toggle(|v| Message::from(AdminMessage::ToggleRegistration(v)));

    items = items.push(public_chars);
    items = items.push(registration);

    let save_btn = button(text("Save policy").size(14))
        .padding(Padding::from([6, 16]))
        .on_press(Message::from(AdminMessage::SaveBrokerConfig));

    items = items.push(save_btn);

    items.into()
}

/// Users Tab: user creation form, user cards with token metrics, promote/demote and delete buttons.
pub fn users_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![].spacing(14).width(Length::Fill);

    // 1. Create User Section
    items = items.push(text("Create User").size(16));

    let username_input = text_input("Username", state.admin_new_username)
        .on_input(|s| Message::from(AdminMessage::NewUsernameChanged(s)))
        .width(Length::Fixed(180.0));

    let password_input = text_input("Password", state.admin_new_password)
        .secure(true)
        .on_input(|s| Message::from(AdminMessage::NewPasswordChanged(s)))
        .width(Length::Fixed(180.0));

    let role_picker = pick_list(
        &[AdminRoleOption::User, AdminRoleOption::Admin][..],
        Some(AdminRoleOption::from(state.admin_new_role)),
        |r: AdminRoleOption| Message::from(AdminMessage::NewRoleChanged(r.into())),
    )
    .width(Length::Fixed(120.0));

    let create_btn = button(text("Create"))
        .padding(Padding::from([5, 12]))
        .on_press(Message::from(AdminMessage::CreateUser));

    let create_row = row![username_input, password_input, role_picker, create_btn]
        .spacing(8)
        .align_y(Alignment::Center);

    items = items.push(create_row.wrap());
    items = items.push(rule::horizontal(1));

    // 2. User Cards Section
    items = items.push(text(format!("Users ({})", state.users.len())).size(16));

    if state.users.is_empty() {
        items = items.push(
            text("No users")
                .size(13)
                .color(Color::from_rgba(0.6, 0.6, 0.6, 1.0)),
        );
    } else {
        for user in state.users {
            let role_label = match user.role {
                Role::Admin => "Admin",
                Role::User => "User",
            };

            let mut card_row = row![
                text(&user.username).size(14),
                text(role_label)
                    .size(12)
                    .color(Color::from_rgba(0.5, 0.5, 0.5, 1.0)),
                hint(
                    text(format!("{} tokens", format_token_count(user.usage.total()))).size(12),
                    format!(
                        "{} prompt · {} completion",
                        format_token_count(user.usage.prompt_tokens),
                        format_token_count(user.usage.completion_tokens)
                    )
                ),
                Space::new().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center);

            if user.id != state.current_user_id {
                let (next_role, action_label) = if user.role == Role::Admin {
                    (Role::User, "Demote")
                } else {
                    (Role::Admin, "Promote")
                };

                let user_id = user.id.clone();
                let role_btn = button(text(action_label).size(12))
                    .padding(Padding::from([3, 8]))
                    .on_press(Message::from(AdminMessage::SetUserRole {
                        user_id: user_id.clone(),
                        role: next_role,
                    }));

                let del_btn = button(text("Delete").size(12))
                    .padding(Padding::from([3, 8]))
                    .on_press(Message::from(AdminMessage::DeleteUser(user_id)));

                card_row = card_row.push(role_btn).push(del_btn);
            }

            let card = ui::card(card_row.wrap(), state.glass_mode);
            items = items.push(card);
        }
    }

    items.into()
}

fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::primary(theme, status);
    style.text_color = ui::COLOR_ON_PRIMARY;
    style
}

/// Keep optional guidance out of the default layout.
fn hint<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    help: impl Into<std::borrow::Cow<'a, str>>,
) -> Element<'a, Message> {
    let help: std::borrow::Cow<'a, str> = help.into();
    iced::widget::tooltip(
        content,
        container(text(help).size(12))
            .padding(8)
            .max_width(300)
            .style(ui::tooltip_style),
        iced::widget::tooltip::Position::Top,
    )
    .into()
}

#[path = "model_settings.rs"]
mod model_settings;

#[path = "image_settings.rs"]
mod image_settings;

#[path = "model_manager.rs"]
mod model_manager;
use model_manager::ollama_tab_view;

/// Logs Tab: broker log viewer with search and size overview.
pub fn logs_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![].spacing(10).width(Length::Fill);

    let mut header_actions = row![
        text("Broker Log").size(16),
        Space::new().width(Length::Fill),
        button(text("Refresh")).on_press(Message::from(AdminMessage::RefreshLogs)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if !state.admin_log.content.is_empty() {
        header_actions = header_actions
            .push(button(text("Copy all")).on_press(Message::from(AdminMessage::CopyLogs)));
    }

    items = items.push(header_actions);

    items = items.push(hint(
        text("Private chat data · current boot")
            .size(12)
            .color(ui::SECONDARY_TEXT),
        "Includes AI prompts and private chat data. Restarting the broker clears this log.",
    ));

    let search_bar = text_input("Filter log lines...", state.admin_log_search)
        .on_input(|s| Message::from(AdminMessage::SearchLogsChanged(s)))
        .width(Length::Fill);
    items = items.push(search_bar);

    let size_str = human_bytes(state.admin_log.file_size_bytes);
    let size_label = text(if state.admin_log.truncated {
        format!("{size_str} on disk · showing newest 512 KiB")
    } else {
        format!("{size_str} on disk · complete current-boot log")
    })
    .size(11)
    .color(Color::from_rgba(0.5, 0.5, 0.5, 1.0));
    items = items.push(size_label);

    let filter = state.admin_log_search.trim().to_lowercase();
    let matching_lines: Vec<&str> = if filter.is_empty() {
        state.admin_log.content.lines().collect()
    } else {
        state
            .admin_log
            .content
            .lines()
            .filter(|line| line.to_lowercase().contains(&filter))
            .collect()
    };

    let log_view: Element<'a, Message> = if matching_lines.is_empty() {
        text(if filter.is_empty() {
            "No entries"
        } else {
            "No matches"
        })
        .size(12)
        .into()
    } else {
        let mut lines_col = column![].spacing(2).width(Length::Fill);
        for line in matching_lines {
            lines_col = lines_col.push(text(line).font(iced::Font::MONOSPACE).size(11));
        }
        lines_col.into()
    };

    let glass_mode = state.glass_mode;
    let log_box = container(log_view)
        .padding(10)
        .width(Length::Fill)
        .style(move |_| ui::card_style(glass_mode));

    items = items.push(log_box);

    items.into()
}

/// Data Tab: database tables explorer with character public toggles.
pub fn data_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![
        row![
            text(format!("{} records", state.admin_data.len())).size(16),
            Space::new().width(Length::Fill),
            button(text("Refresh")).on_press(Message::from(AdminMessage::RefreshData)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
    ]
    .spacing(10)
    .width(Length::Fill);
    for record in state.admin_data {
        let mut title = row![
            text(&record.label).size(15).wrapping(text::Wrapping::Glyph),
            text(&record.kind).size(12).color(ui::SECONDARY_TEXT),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if let Some(is_public) = record.is_public {
            let character_id = record.id.clone();
            title = title.push(
                toggler(is_public)
                    .label("Public")
                    .on_toggle(move |is_public| {
                        Message::from(AdminMessage::ToggleCharacterPublic {
                            character_id: character_id.clone(),
                            is_public,
                        })
                    }),
            );
        }
        let card = column![
            title.wrap(),
            text(&record.detail)
                .size(13)
                .wrapping(text::Wrapping::Glyph)
        ]
        .spacing(6);
        items = items.push(ui::card(card, state.glass_mode));
    }
    if state.admin_data.is_empty() {
        items = items.push(text("No records").size(13));
    }
    items.into()
}

// -----------------------------------------------------------------------------
// Unit Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn runtime_adapter_selection_is_saved_and_respects_operator_override() {
        let mut state = AdminState {
            adapter_plugins: vec!["freetoken".into(), "ollama".into()],
            ..AdminState::default()
        };
        for id in ["ollama", "freetoken"] {
            assert!(
                state
                    .update(AdminMessage::SelectRuntimeAdapter(id.into()), "admin")
                    .is_empty()
            );
            assert_eq!(state.broker_config.adapter_plugin_id(), id);
            let requests = state.update(AdminMessage::SaveBrokerConfig, "admin");
            assert!(
                matches!(&requests[0], Request::AdminSetBrokerConfig { config, .. }
                if config.adapter_plugin == id)
            );
        }
        state.update(
            AdminMessage::SelectRuntimeAdapter("missing".into()),
            "admin",
        );
        assert_eq!(state.broker_config.adapter_plugin, "freetoken");
        state.adapter_override = Some("ollama".into());
        state.update(AdminMessage::SelectRuntimeAdapter("ollama".into()), "admin");
        assert_eq!(state.broker_config.adapter_plugin, "freetoken");
    }

    #[test]
    fn model_settings_navigation_and_disclosure_preserve_saved_values() {
        let mut state = AdminState::default();
        state.broker_config.model = "custom-rp-model".into();
        state.broker_config.num_ctx = 32768;
        state.broker_config.top_k = 20;
        state.broker_config.seed = 42;
        let expected = state.broker_config.clone();
        assert!(
            state
                .update(AdminMessage::ModelsPage(2), "admin")
                .is_empty()
        );
        assert_eq!(state.admin_tab, 2);
        for advanced in [false, true] {
            assert!(
                state
                    .update(AdminMessage::ToggleAdvancedSettings(advanced), "admin")
                    .is_empty()
            );
            assert_eq!(state.broker_config, expected);
            let _view: Element<'_, AdminMessage> = model_settings::view(&state.as_view_state());
        }
        let saved = state.update(AdminMessage::SaveBrokerConfig, "admin");
        assert!(
            matches!(&saved[..], [Request::AdminSetBrokerConfig { config, .. }] if config == &expected)
        );
        for page in 0..4 {
            state.update(AdminMessage::ModelsPage(page), "admin");
            let _view: Element<'_, AdminMessage> =
                model_manager::ollama_tab_view(&state.as_view_state());
        }
    }

    #[test]
    fn clearing_download_history_keeps_active_and_cancelling_downloads() {
        let mut state = AdminState::default();
        for model in ["finished", "active", "cancelling"] {
            state.update(AdminMessage::DownloadModel(model.into()), "admin");
        }
        state.models.downloads.get_mut("finished").unwrap().finished = true;
        state.models.cancel_download("cancelling");
        assert!(
            state
                .update(AdminMessage::ClearFinishedDownloads, "admin")
                .is_empty()
        );
        assert_eq!(state.models.downloads.len(), 2);
        assert!(state.models.is_downloading("active"));
        assert!(state.models.downloads["cancelling"].cancelling);
    }

    #[test]
    fn test_admin_message_variants() {
        let msg = AdminMessage::SelectTab(2);
        assert_eq!(msg, AdminMessage::SelectTab(2));

        let msg = AdminMessage::RefreshMonitor;
        assert_eq!(msg, AdminMessage::RefreshMonitor);

        let msg = AdminMessage::SoftReboot;
        assert_eq!(msg, AdminMessage::SoftReboot);

        let msg = AdminMessage::SaveBrokerConfig;
        assert_eq!(msg, AdminMessage::SaveBrokerConfig);

        let msg = AdminMessage::ToggleAdapter(true);
        assert_eq!(msg, AdminMessage::ToggleAdapter(true));

        let msg = AdminMessage::AdapterUrlChanged("http://localhost:11434".into());
        assert_eq!(
            msg,
            AdminMessage::AdapterUrlChanged("http://localhost:11434".into())
        );

        let msg = AdminMessage::SelectRuntimeAdapter("ollama".into());
        assert_eq!(msg, AdminMessage::SelectRuntimeAdapter("ollama".into()));

        let msg = AdminMessage::SelectAdapterModel("llama3.2".into());
        assert_eq!(msg, AdminMessage::SelectAdapterModel("llama3.2".into()));

        let msg = AdminMessage::TemperatureChanged(0.7);
        assert_eq!(msg, AdminMessage::TemperatureChanged(0.7));

        let msg = AdminMessage::TopPChanged(0.95);
        assert_eq!(msg, AdminMessage::TopPChanged(0.95));

        let msg = AdminMessage::TopKChanged(50);
        assert_eq!(msg, AdminMessage::TopKChanged(50));

        let msg = AdminMessage::NumCtxChanged(8192);
        assert_eq!(msg, AdminMessage::NumCtxChanged(8192));

        let msg = AdminMessage::NumPredictChanged(512);
        assert_eq!(msg, AdminMessage::NumPredictChanged(512));

        let msg = AdminMessage::RepeatPenaltyChanged(1.15);
        assert_eq!(msg, AdminMessage::RepeatPenaltyChanged(1.15));

        let msg = AdminMessage::SeedChanged(42);
        assert_eq!(msg, AdminMessage::SeedChanged(42));

        let msg = AdminMessage::KeepAliveChanged("10m".into());
        assert_eq!(msg, AdminMessage::KeepAliveChanged("10m".into()));

        let msg = AdminMessage::TogglePublicCharacters(true);
        assert_eq!(msg, AdminMessage::TogglePublicCharacters(true));

        let msg = AdminMessage::ToggleRegistration(false);
        assert_eq!(msg, AdminMessage::ToggleRegistration(false));

        let msg = AdminMessage::RefreshOllama;
        assert_eq!(msg, AdminMessage::RefreshOllama);

        let msg = AdminMessage::OllamaPullModelChanged("mistral".into());
        assert_eq!(msg, AdminMessage::OllamaPullModelChanged("mistral".into()));

        let msg = AdminMessage::OllamaPull;
        assert_eq!(msg, AdminMessage::OllamaPull);

        let msg = AdminMessage::OllamaLoad("mistral".into());
        assert_eq!(msg, AdminMessage::OllamaLoad("mistral".into()));

        let msg = AdminMessage::OllamaUnload("mistral".into());
        assert_eq!(msg, AdminMessage::OllamaUnload("mistral".into()));

        let msg = AdminMessage::OllamaDelete("mistral".into());
        assert_eq!(msg, AdminMessage::OllamaDelete("mistral".into()));

        let msg = AdminMessage::NewUsernameChanged("alice".into());
        assert_eq!(msg, AdminMessage::NewUsernameChanged("alice".into()));

        let msg = AdminMessage::NewPasswordChanged("secret".into());
        assert_eq!(msg, AdminMessage::NewPasswordChanged("secret".into()));

        let msg = AdminMessage::NewRoleChanged(Role::Admin);
        assert_eq!(msg, AdminMessage::NewRoleChanged(Role::Admin));

        let msg = AdminMessage::CreateUser;
        assert_eq!(msg, AdminMessage::CreateUser);

        let msg = AdminMessage::set_user_role("u1", Role::Admin);
        assert_eq!(
            msg,
            AdminMessage::SetUserRole {
                user_id: "u1".into(),
                role: Role::Admin,
            }
        );

        let msg = AdminMessage::DeleteUser("u1".into());
        assert_eq!(msg, AdminMessage::DeleteUser("u1".into()));

        let msg = AdminMessage::RefreshLogs;
        assert_eq!(msg, AdminMessage::RefreshLogs);

        let msg = AdminMessage::SearchLogsChanged("error".into());
        assert_eq!(msg, AdminMessage::SearchLogsChanged("error".into()));

        let msg = AdminMessage::CopyLogs;
        assert_eq!(msg, AdminMessage::CopyLogs);

        let msg = AdminMessage::RefreshData;
        assert_eq!(msg, AdminMessage::RefreshData);

        let msg = AdminMessage::toggle_character_public("c1", true);
        assert_eq!(
            msg,
            AdminMessage::ToggleCharacterPublic {
                character_id: "c1".into(),
                is_public: true,
            }
        );

        let msg = AdminMessage::CloseDialog;
        assert_eq!(msg, AdminMessage::CloseDialog);
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(125), "2m 5s");
        assert_eq!(format_duration(3665), "1h 1m 5s");
        assert_eq!(format_duration(90065), "1d 1h 1m");
    }

    #[test]
    fn test_human_bytes() {
        assert_eq!(human_bytes(500), "500 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(10 * 1024 * 1024), "10.0 MiB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn test_format_token_count() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1000), "1,000");
        assert_eq!(format_token_count(1234567), "1,234,567");
    }

    #[test]
    fn test_requests_for_tab() {
        let token = "test_tok";

        let r0 = requests_for_tab(0, token);
        assert_eq!(r0.len(), 2);
        assert!(matches!(r0[0], Request::AdminGetBrokerConfig { .. }));
        assert!(matches!(r0[1], Request::AdminGetBrokerMonitor { .. }));

        let r1 = requests_for_tab(1, token);
        assert_eq!(r1.len(), 1);
        assert!(matches!(r1[0], Request::AdminListUsers { .. }));

        let r2 = requests_for_tab(2, token);
        assert_eq!(r2.len(), 2);
        assert!(matches!(r2[0], Request::AdminGetOllamaState { .. }));

        let r3 = requests_for_tab(3, token);
        assert_eq!(r3.len(), 1);
        assert!(matches!(r3[0], Request::AdminGetBrokerConfig { .. }));

        let r4 = requests_for_tab(4, token);
        assert_eq!(r4.len(), 1);
        assert!(matches!(r4[0], Request::AdminReadBrokerLog { .. }));

        let r5 = requests_for_tab(5, token);
        assert_eq!(r5.len(), 1);
        assert!(matches!(r5[0], Request::AdminReadDatabase { .. }));
    }

    #[test]
    fn workflow_files_are_checked_and_summarised() {
        let api = r#"{"1": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}"}},
            "2": {"class_type": "KSampler", "inputs": {"seed": "{{seed}}"}}}"#;
        assert!(check_image_workflow(api).is_ok());
        assert!(
            check_image_workflow(r#"{"nodes": [], "links": []}"#)
                .unwrap_err()
                .contains("Export (API)")
        );
        assert!(
            check_image_workflow(r#"{"1": {}}"#)
                .unwrap_err()
                .contains("{{prompt}}")
        );
        assert!(check_image_workflow("{").is_err());
        assert!(workflow_summary("").starts_with("Built-in"));
        assert_eq!(
            workflow_summary(api),
            "Custom workflow: 2 nodes, 1 KiB. Fills {{prompt}}, {{seed}}."
        );
        let mut state = AdminState::default();
        let mut config = state.broker_config.clone();
        config.image.workflow = api.into();
        state.set_broker_config(config);
        assert!(state.image_workflow_summary.contains("2 nodes"));
        state.update(AdminMessage::ClearImageWorkflow, "t");
        assert!(state.broker_config.image.workflow.is_empty());
    }

    #[test]
    fn image_test_uses_default_prompt_and_runs_once() {
        let mut state = AdminState::default();
        let requests = state.update(AdminMessage::TestImageGeneration, "t");
        assert!(matches!(
            requests.as_slice(),
            [Request::GenerateImage { prompt, square: false, .. }] if prompt == DEFAULT_TEST_PROMPT
        ));
        assert!(state.image_test.pending);
        assert!(
            state
                .update(AdminMessage::TestImageGeneration, "t")
                .is_empty()
        );
    }

    #[test]
    fn image_settings_are_saved_with_broker_config() {
        let mut state = AdminState {
            image_plugins: vec!["comfyui".into()],
            image_models: Some(vec!["old.safetensors".into()]),
            ..AdminState::default()
        };
        state.update(AdminMessage::ToggleImages(true), "t");
        state.update(
            AdminMessage::ImageUrlChanged("http://192.168.0.97:8188".into()),
            "t",
        );
        assert!(
            state.image_models.is_none(),
            "a new server needs a new model list"
        );
        state.update(AdminMessage::SelectImagePlugin("unknown".into()), "t");
        state.update(AdminMessage::ImageStepsChanged(30), "t");
        let requests = state.update(AdminMessage::SaveBrokerConfig, "t");
        let [Request::AdminSetBrokerConfig { config, .. }] = requests.as_slice() else {
            panic!("expected a config save")
        };
        assert!(config.image.enabled);
        assert_eq!(config.image.url, "http://192.168.0.97:8188");
        assert_eq!(
            config.image.plugin, "comfyui",
            "unknown plugins are ignored"
        );
        assert_eq!(config.image.steps, 30);
        assert!(matches!(
            state.update(AdminMessage::LoadImageModels, "t").as_slice(),
            [Request::AdminListImageModels { .. }]
        ));
    }

    #[test]
    fn test_admin_subscription_gating() {
        // When active is false, subscription must be none (zero timers)
        let sub_inactive = admin_subscription(false);
        let _ = sub_inactive;

        // When active is true, subscription is generated
        let sub_active = admin_subscription(true);
        let _ = sub_active;

        // Gated helper test
        let sub_gated_off = admin_subscription_gated(false, 0);
        let _ = sub_gated_off;

        let sub_gated_tab1 = admin_subscription_gated(true, 1);
        let _ = sub_gated_tab1;

        let sub_gated_on = admin_subscription_gated(true, 0);
        let _ = sub_gated_on;
    }

    #[test]
    fn test_admin_state_update() {
        let mut state = AdminState::default();
        let token = "tok123";

        // Tab selection
        let reqs = state.update(AdminMessage::SelectTab(1), token);
        assert_eq!(state.admin_tab, 1);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(reqs[0], Request::AdminListUsers { .. }));

        // Broker monitor refresh
        let reqs = state.update(AdminMessage::RefreshMonitor, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(reqs[0], Request::AdminGetBrokerMonitor { .. }));

        // Soft reboot
        let reqs = state.update(AdminMessage::SoftReboot, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(reqs[0], Request::AdminSoftReboot { .. }));

        // Save config
        let reqs = state.update(AdminMessage::SaveBrokerConfig, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(reqs[0], Request::AdminSetBrokerConfig { .. }));

        // Adapter controls
        state.update(AdminMessage::ToggleAdapter(true), token);
        assert!(state.broker_config.adapter_enabled);

        state.update(
            AdminMessage::AdapterUrlChanged("http://remote:11434".into()),
            token,
        );
        assert_eq!(state.broker_config.adapter_url, "http://remote:11434");

        state.adapter_plugins = vec!["ollama".into(), "openai-compatible".into()];
        state.update(AdminMessage::SelectRuntimeAdapter("ollama".into()), token);
        assert!(state.broker_config.use_ollama_api);

        state.update(AdminMessage::SelectAdapterModel("llama3.2".into()), token);
        assert_eq!(state.broker_config.model, "llama3.2");

        state.update(AdminMessage::TemperatureChanged(1.2), token);
        assert_eq!(state.broker_config.temperature, 1.2);

        state.update(AdminMessage::TopPChanged(0.85), token);
        assert_eq!(state.broker_config.top_p, 0.85);

        state.update(AdminMessage::TopKChanged(60), token);
        assert_eq!(state.broker_config.top_k, 60);

        state.update(AdminMessage::NumCtxChanged(8192), token);
        assert_eq!(state.broker_config.num_ctx, 8192);

        state.update(AdminMessage::NumPredictChanged(1024), token);
        assert_eq!(state.broker_config.num_predict, 1024);

        state.update(AdminMessage::RepeatPenaltyChanged(1.2), token);
        assert_eq!(state.broker_config.repeat_penalty, 1.2);

        state.update(AdminMessage::SeedChanged(999), token);
        assert_eq!(state.broker_config.seed, 999);

        state.update(AdminMessage::KeepAliveChanged("15m".into()), token);
        assert_eq!(state.broker_config.keep_alive, "15m");

        state.update(AdminMessage::TogglePublicCharacters(true), token);
        assert!(state.broker_config.allow_public_characters);

        state.update(AdminMessage::ToggleRegistration(false), token);
        assert!(!state.broker_config.allow_self_registration);

        // Ollama actions
        let reqs = state.update(AdminMessage::RefreshOllama, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(reqs[0], Request::AdminGetOllamaState { .. }));

        state.update(AdminMessage::OllamaPullModelChanged("llama3".into()), token);
        assert_eq!(state.ollama_pull_model, "llama3");

        state.models.busy = false;
        let reqs = state.update(AdminMessage::OllamaPull, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(
            &reqs[0],
            Request::AdminOllamaAction {
                action: OllamaAction::Pull { model },
                ..
            } if model == "llama3"
        ));

        state.models.busy = false;
        let reqs = state.update(AdminMessage::OllamaLoad("qwen".into()), token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(
            &reqs[0],
            Request::AdminOllamaAction {
                action: OllamaAction::Load { model },
                ..
            } if model == "qwen"
        ));

        state.models.busy = false;
        let reqs = state.update(AdminMessage::OllamaUnload("qwen".into()), token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(
            &reqs[0],
            Request::AdminOllamaAction {
                action: OllamaAction::Unload { model },
                ..
            } if model == "qwen"
        ));

        state.models.busy = false;
        state.update(AdminMessage::ConfirmModelDelete("qwen".into()), token);
        let reqs = state.update(AdminMessage::OllamaDelete("qwen".into()), token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(
            &reqs[0],
            Request::AdminOllamaAction {
                action: OllamaAction::Delete { model },
                ..
            } if model == "qwen"
        ));

        // User management
        state.update(AdminMessage::NewUsernameChanged("bob".into()), token);
        assert_eq!(state.admin_new_username, "bob");

        state.update(AdminMessage::NewPasswordChanged("pwd123".into()), token);
        assert_eq!(state.admin_new_password, "pwd123");

        state.update(AdminMessage::NewRoleChanged(Role::Admin), token);
        assert_eq!(state.admin_new_role, Role::Admin);

        let reqs = state.update(AdminMessage::CreateUser, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(
            &reqs[0],
            Request::AdminCreateUser {
                username,
                password,
                role: Role::Admin,
                ..
            } if username == "bob" && password == "pwd123"
        ));
        assert!(state.admin_new_password.is_empty());

        let reqs = state.update(
            AdminMessage::SetUserRole {
                user_id: "u2".into(),
                role: Role::User,
            },
            token,
        );
        assert_eq!(reqs.len(), 1);
        assert!(
            matches!(&reqs[0], Request::AdminSetRole { user_id, role: Role::User, .. } if user_id == "u2")
        );

        let reqs = state.update(AdminMessage::DeleteUser("u2".into()), token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(
            &reqs[0],
            Request::AdminDeleteUser { user_id, .. } if user_id == "u2"
        ));

        // Logs & Data
        let reqs = state.update(AdminMessage::RefreshLogs, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(reqs[0], Request::AdminReadBrokerLog { .. }));

        state.update(AdminMessage::SearchLogsChanged("warn".into()), token);
        assert_eq!(state.admin_log_search, "warn");

        let reqs = state.update(AdminMessage::RefreshData, token);
        assert_eq!(reqs.len(), 1);
        assert!(matches!(reqs[0], Request::AdminReadDatabase { .. }));

        let reqs = state.update(
            AdminMessage::ToggleCharacterPublic {
                character_id: "char_abc".into(),
                is_public: true,
            },
            token,
        );
        assert_eq!(reqs.len(), 1);
        assert!(matches!(
            &reqs[0],
            Request::AdminSetCharacterPublic {
                character_id,
                is_public: true,
                ..
            } if character_id == "char_abc"
        ));
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_admin_dialog_view_rendering() {
        let mut state = AdminState::default();

        // Sample data for rendering
        state.broker_monitor = Some(BrokerMonitor {
            uptime_seconds: 3661,
            cpu_percent: 12.5,
            memory_used_mb: 256,
            memory_limit_mb: Some(1024),
            active_connections: 3,
            adapter_status: AdapterStatus::Online,
            adapter_model_count: 2,
            adapter_latency_ms: Some(42),
            recent_errors: vec!["Sample error line".into()],
        });

        state.ollama_state = Some(OllamaState {
            version: "0.3.14".into(),
            models: vec![chatty_protocol::OllamaModel {
                name: "llama3.2:latest".into(),
                size: 2_000_000_000,
                modified_at: "now".into(),
                family: "llama".into(),
                parameter_size: "3.2B".into(),
                quantization_level: "Q4_K_M".into(),
            }],
            running_models: vec![chatty_protocol::OllamaRunningModel {
                name: "llama3.2:latest".into(),
                size: 2_000_000_000,
                size_vram: 1_800_000_000,
                expires_at: "in 4 minutes".into(),
            }],
        });

        state.users = vec![UserAccount {
            id: "user-1".into(),
            username: "admin".into(),
            role: Role::Admin,
            created_at: "2025-01-01".into(),
            usage: chatty_protocol::TokenUsage {
                prompt_tokens: 1000,
                completion_tokens: 500,
            },
        }];
        state.current_user_id = "user-1".into();

        state.admin_log = BrokerLog {
            content: "2026-10-14 INFO Broker started\n2026-10-14 WARN Connection dropped\n".into(),
            truncated: false,
            file_size_bytes: 84,
        };

        state.admin_data = vec![AdminDataRow {
            kind: "Character".into(),
            id: "c1".into(),
            label: "Lyra".into(),
            detail: "A noble warrior".into(),
            is_public: Some(true),
        }];

        // Test rendering every tab
        for tab in 0..ADMIN_TAB_NAMES.len() {
            state.admin_tab = tab;
            let view_state = state.as_view_state();
            let _element: Element<'_, AdminMessage> = admin_dialog_view(&view_state);
        }
    }
    #[test]
    fn model_download_keeps_file_choice_and_blocks_duplicates() {
        let mut state = AdminState::default();
        let name = "hf.co/owner/chat-GGUF:chat-Q4_K_M.gguf";
        let requests = state.update(AdminMessage::DownloadModel(name.into()), "admin-session");
        assert!(matches!(&requests[..], [Request::AdminOllamaAction {
            session_token, action: OllamaAction::Pull { model }
        }] if session_token == "admin-session" && model == name));
        assert!(!state.models.busy);
        assert!(
            state
                .update(AdminMessage::DownloadModel(name.into()), "admin-session")
                .is_empty()
        );
        assert_eq!(
            state
                .update(AdminMessage::DownloadModel("other".into()), "admin-session")
                .len(),
            1
        );
        assert_eq!(state.models.downloads.len(), 2);
    }

    #[test]
    fn parallel_download_limit_does_not_block_unrelated_deletion() {
        let mut state = AdminState::default();
        for model in ["a", "b", "c", "d"] {
            assert_eq!(
                state
                    .update(AdminMessage::DownloadModel(model.into()), "session")
                    .len(),
                1
            );
        }
        assert!(
            state
                .update(AdminMessage::DownloadModel("overflow".into()), "session")
                .is_empty()
        );
        state.update(AdminMessage::ConfirmModelDelete("a".into()), "session");
        assert!(
            state
                .update(AdminMessage::OllamaDelete("a".into()), "session")
                .is_empty()
        );
        state.update(
            AdminMessage::ConfirmModelDelete("unrelated".into()),
            "session",
        );
        assert_eq!(
            state
                .update(AdminMessage::OllamaDelete("unrelated".into()), "session")
                .len(),
            1
        );
        state.models.download_started("a", 1);
        state.models.download_for_request(1).unwrap().finished = true;
        assert_eq!(
            state
                .update(AdminMessage::DownloadModel("overflow".into()), "session")
                .len(),
            1
        );
        state.models.page = 1;
        let _view: Element<'_, AdminMessage> =
            super::model_manager::ollama_tab_view(&state.as_view_state());
    }

    #[test]
    fn model_delete_requires_confirmation_and_preserves_saved_model() {
        let mut state = AdminState::default();
        assert!(
            state
                .update(AdminMessage::OllamaDelete("model".into()), "session")
                .is_empty()
        );
        state.update(AdminMessage::ConfirmModelDelete("model".into()), "session");
        state.update(AdminMessage::CancelModelDelete, "session");
        assert!(
            state
                .update(AdminMessage::OllamaDelete("model".into()), "session")
                .is_empty()
        );
        state.models.saved_model = "model".into();
        state.update(
            AdminMessage::ConfigureModel("replacement".into()),
            "session",
        );
        state.update(AdminMessage::ConfirmModelDelete("model".into()), "session");
        assert!(
            state
                .update(AdminMessage::OllamaDelete("model".into()), "session")
                .is_empty()
        );
        state.models.saved_model = "replacement".into();
        assert_eq!(
            state
                .update(AdminMessage::OllamaDelete("model".into()), "session")
                .len(),
            1
        );
    }
    #[test]
    fn model_download_requires_saved_server_changes_and_survives_tab_switches() {
        let mut state = AdminState::default();
        state.models.saved_adapter_url = Some("http://old-server:11434".into());
        state.broker_config.adapter_url = "http://inference-server:11434".into();
        assert!(
            state
                .update(AdminMessage::DownloadModel("llama3.2:3b".into()), "session")
                .is_empty()
        );
        assert!(!state.models.busy);
        state.models.saved_adapter_url = Some(state.broker_config.adapter_url.clone());
        assert_eq!(
            state
                .update(AdminMessage::DownloadModel("llama3.2:3b".into()), "session")
                .len(),
            1
        );
        state.models.download_started("llama3.2:3b", 99);
        state.update(AdminMessage::SelectTab(1), "session");
        assert_eq!(state.update(AdminMessage::SelectTab(2), "session").len(), 2);
        assert_eq!(state.models.downloads["llama3.2:3b"].request_id, Some(99));
        assert!(!state.models.busy);
    }
}
