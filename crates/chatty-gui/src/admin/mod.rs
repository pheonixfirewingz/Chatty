//! Admin monitor, broker metrics, user management, Ollama controls, log viewer, and database explorer built with Iced 0.14.
//!
//! Provides Elm-architecture messages ([`AdminMessage`]), Iced view components ([`admin_dialog_view`]),
//! state management ([`AdminViewState`], [`AdminState`]), and 2-second telemetry subscription ([`admin_subscription`]).

mod image_settings;
mod model_manager;
mod model_settings;
#[cfg(test)]
mod tests;
mod view;

pub use view::*;

use chatty_protocol::{
    AdminDataRow, BrokerConfig, BrokerLog, BrokerMonitor, OllamaAction, OllamaState, Request, Role,
    UserAccount,
};
use std::time::{Duration, Instant};

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
