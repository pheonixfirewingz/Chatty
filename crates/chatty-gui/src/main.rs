#![allow(clippy::collapsible_if)]

mod admin_monitor;
pub mod characters;
mod conversation;
pub mod memories;
mod network;
mod tts;
mod ui;
pub mod worlds;

use chatty_protocol::util::args::ParsedArgs;
use chatty_protocol::util::current_utc_timestamp;
use chatty_protocol::util::{Error, Result, format_err};
use chatty_protocol::*;
use iced::futures::StreamExt;
use iced::futures::stream::BoxStream;
use iced::widget::{
    Id, Space, button, column, container, mouse_area, responsive, row, scrollable, stack, text,
    text_input, toggler, tooltip,
};
use iced::{
    Alignment, Background, Border, Color, Element, Length, Padding, Point, Size, Subscription,
    Task, Theme, font, window,
};
use iced_dialog::dialog;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use admin_monitor::{AdminMessage, AdminState, admin_dialog_view, admin_subscription};
use characters::{CharacterMessage, DraftCharacter, character_dialog_view};
use conversation::{
    ConversationMessage, ConversationViewState, NewChatViewState, conversation_view,
    new_chat_dialog_view,
};
use memories::{
    MemoryAction, MemoryDialogState, MemoryMessage, MemoryViewState, memory_dialog_view,
};
use network::{
    Command, ConnectionTarget, Event, PendingCertificate, save_preferences, session_path,
};
use tts::{LocalTts, TtsEvent, test_speech, tts_channel};
use ui::{FooterIcon, icon_button, sidebar_panel, sidebar_toggle_button};
use worlds::{WorldAction, WorldDialogState, WorldMessage, WorldViewState, world_dialog_view};

const USAGE: &str = "chatty-gui -- Chatty client\n\n\
Options:\n\
  --broker <addr>         Broker address [env: CHATTY_BROKER]\n\
  --ca <path>             Pinned CA certificate PEM [env: CHATTY_CA]\n\
  --inspect               Open the inspection harness instead of the app\n\
  --inspect-control <fifo> FIFO for inspection control commands [env: CHATTY_INSPECT_CONTROL]\n\
  --width <px>            Initial window width [default: 1100]\n\
  --height <px>           Initial window height [default: 720]\n\
  --session-file <path>   Session storage path [env: CHATTY_SESSION_FILE]\n\
  --test-speech           Play a short test phrase and exit\n";

#[derive(Clone, Debug)]
pub struct UiNotice {
    pub timestamp: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Chat,
    Admin,
    Settings,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) enum Message {
    Network(Event),
    Tts(TtsEvent),
    Admin(AdminMessage),
    Character(CharacterMessage),
    World(WorldMessage),
    Memory(MemoryMessage),
    Conversation(ConversationMessage),
    InspectControl(String),
    ScreenshotCaptured(iced::window::Screenshot),
    ServerAddressChanged(String),
    TryConnect,
    LoginUsernameChanged(String),
    LoginPasswordChanged(String),
    SubmitLogin,
    SubmitRegister,
    ChangeServer,
    SelectConversation(String),
    DeleteConversation(String),
    HoverConversation(Option<String>),
    OpenNewChat,
    OpenCharactersDialog,
    CloseCharactersDialog,
    OpenWorldsDialog,
    CloseWorldsDialog,
    OpenMemoriesDialog(Option<String>, Option<String>),
    CloseMemoriesDialog,
    OpenAdminDialog,
    CloseAdminDialog,
    OpenSettingsDialog,
    CloseSettingsDialog,
    SetTtsAutoSpeak(bool),
    SignOut,
    ToggleSidebar,
    ToggleCompactSidebar,
    TrustCertificate(bool),
    DismissError,
    DismissMemoryNotice,
    WindowResized(Size),
    WindowClosed,
}

impl From<AdminMessage> for Message {
    fn from(msg: AdminMessage) -> Self {
        Message::Admin(msg)
    }
}

impl From<WorldMessage> for Message {
    fn from(msg: WorldMessage) -> Self {
        Message::World(msg)
    }
}

impl From<MemoryMessage> for Message {
    fn from(msg: MemoryMessage) -> Self {
        Message::Memory(msg)
    }
}

impl From<CharacterMessage> for Message {
    fn from(msg: CharacterMessage) -> Self {
        Message::Character(msg)
    }
}

impl From<ConversationMessage> for Message {
    fn from(msg: ConversationMessage) -> Self {
        Message::Conversation(msg)
    }
}

#[derive(Clone)]
struct ChannelHolder<T>(
    Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<T>>>>,
    usize,
);

impl<T> std::hash::Hash for ChannelHolder<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.1.hash(state);
    }
}

impl<T> PartialEq for ChannelHolder<T> {
    fn eq(&self, other: &Self) -> bool {
        self.1 == other.1
    }
}

impl<T> Eq for ChannelHolder<T> {}

fn run_channel_stream<T: Send + 'static>(holder: &ChannelHolder<T>) -> BoxStream<'static, T> {
    let mutex = holder.0.clone();
    iced::stream::channel(
        100,
        move |mut output: iced::futures::channel::mpsc::Sender<T>| async move {
            use iced::futures::sink::SinkExt;
            let rx = {
                let mut guard = mutex.lock().await;
                guard.take()
            };
            if let Some(mut rx) = rx {
                while let Some(item) = rx.recv().await {
                    if output.send(item).await.is_err() {
                        break;
                    }
                }
            }
        },
    )
    .boxed()
}

fn run_network_stream(holder: &ChannelHolder<Event>) -> BoxStream<'static, Event> {
    run_channel_stream(holder)
}

fn run_tts_stream(holder: &ChannelHolder<TtsEvent>) -> BoxStream<'static, TtsEvent> {
    run_channel_stream(holder)
}

#[allow(clippy::ptr_arg)]
fn run_fifo_stream(path: &PathBuf) -> BoxStream<'static, String> {
    let path = path.clone();
    iced::stream::channel(
        16,
        move |mut output: iced::futures::channel::mpsc::Sender<String>| async move {
            use iced::futures::sink::SinkExt;
            if !path.exists() {
                let _ = std::process::Command::new("mkfifo").arg(&path).status();
            }
            loop {
                let path_clone = path.clone();
                let line = tokio::task::spawn_blocking(move || {
                    use std::io::BufRead;
                    let Ok(file) = std::fs::File::open(&path_clone) else {
                        return None;
                    };
                    let mut reader = std::io::BufReader::new(file);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                        Some(line)
                    } else {
                        None
                    }
                })
                .await
                .ok()
                .flatten();

                if let Some(line) = line {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        if output.send(trimmed).await.is_err() {
                            break;
                        }
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        },
    )
    .boxed()
}

pub struct ChattyApp {
    commands: mpsc::UnboundedSender<Command>,
    events_holder: ChannelHolder<Event>,
    tts_holder: ChannelHolder<TtsEvent>,
    fifo_path: Option<PathBuf>,
    pending_screenshot_path: Option<PathBuf>,
    zoom: f32,

    status: String,
    status_tooltip: Option<String>,
    server_address: String,
    pending_certificate: Option<PendingCertificate>,
    connected: bool,
    connecting: bool,
    restoring_session: bool,
    token: String,
    user_id: String,
    revision: i64,
    username: String,
    password: String,
    role: Option<Role>,
    registration_enabled: bool,
    state: HashMap<(String, String), DeltaPayload>,
    characters: Vec<Character>,
    character_image_requests: HashSet<(String, String)>,
    received_character_images: Vec<(String, Vec<u8>)>,
    character_search: String,
    worlds: Vec<World>,
    world_state: WorldDialogState,
    worlds_open: bool,
    memories: Vec<MemoryEntry>,
    memory_state: MemoryDialogState,
    memory_dialog_open: bool,
    conversations: Vec<Conversation>,
    messages: Vec<ChatMessage>,
    selected_conversation: Option<String>,
    auto_select_conversation: bool,
    selected_characters: HashSet<String>,
    input: String,
    editing_message_id: Option<String>,
    editing_message_content: String,
    hovered_message_id: Option<String>,
    hovered_conversation_id: Option<String>,
    stream_text: String,
    typing_character: Option<String>,
    generation_status: Option<GenerationStatus>,
    active_request: Option<u64>,
    screen: Screen,
    sidebar_visible: bool,
    compact_sidebar_open: bool,
    error: Option<UiNotice>,
    draft: DraftCharacter,
    draft_world_ids: HashSet<String>,
    draft_character_open: bool,
    char_import_pending: bool,
    char_import_started: Option<Instant>,
    new_chat_open: bool,
    new_conversation_title: String,
    admin: AdminState,
    last_monitor_refresh: Option<Instant>,
    account_usage: TokenUsage,
    light_mode: bool,
    glass_mode: bool,
    preferences_path: Option<PathBuf>,
    tts: LocalTts,
    tts_auto_speak: bool,
    pending_tts_message_id: Option<String>,
}

#[derive(Clone)]
pub struct Args {
    pub broker: String,
    pub ca: Option<PathBuf>,
    pub inspect: bool,
    pub inspect_control: Option<PathBuf>,
    pub width: f32,
    pub height: f32,
    pub session_file: Option<PathBuf>,
    pub test_speech: bool,
}

impl ChattyApp {
    pub(crate) fn new(
        commands: mpsc::UnboundedSender<Command>,
        events_rx: mpsc::UnboundedReceiver<Event>,
    ) -> Self {
        let (tts, tts_rx) = tts_channel();
        Self {
            commands,
            events_holder: ChannelHolder(Arc::new(tokio::sync::Mutex::new(Some(events_rx))), 1),
            tts_holder: ChannelHolder(Arc::new(tokio::sync::Mutex::new(Some(tts_rx))), 2),
            fifo_path: None,
            pending_screenshot_path: None,
            zoom: 1.0,

            status: "Enter the server IP to begin.".into(),
            status_tooltip: None,
            server_address: String::new(),
            pending_certificate: None,
            connected: false,
            connecting: false,
            restoring_session: false,
            token: String::new(),
            user_id: String::new(),
            revision: 0,
            username: String::new(),
            password: String::new(),
            role: None,
            registration_enabled: true,
            state: HashMap::new(),
            characters: Vec::new(),
            character_image_requests: HashSet::new(),
            received_character_images: Vec::new(),
            character_search: String::new(),
            worlds: Vec::new(),
            world_state: WorldDialogState::default(),
            worlds_open: false,
            memories: Vec::new(),
            memory_state: MemoryDialogState::default(),
            memory_dialog_open: false,
            conversations: Vec::new(),
            messages: Vec::new(),
            selected_conversation: None,
            auto_select_conversation: true,
            selected_characters: HashSet::new(),
            input: String::new(),
            editing_message_id: None,
            editing_message_content: String::new(),
            hovered_message_id: None,
            hovered_conversation_id: None,
            stream_text: String::new(),
            typing_character: None,
            generation_status: None,
            active_request: None,
            screen: Screen::Login,
            sidebar_visible: true,
            compact_sidebar_open: false,
            error: None,
            draft: DraftCharacter::default(),
            draft_world_ids: HashSet::new(),
            draft_character_open: false,
            char_import_pending: false,
            char_import_started: None,
            new_chat_open: false,
            new_conversation_title: String::new(),
            admin: AdminState::default(),
            last_monitor_refresh: None,
            account_usage: TokenUsage::default(),
            light_mode: false,
            glass_mode: true,
            preferences_path: None,
            tts,
            tts_auto_speak: false,
            pending_tts_message_id: None,
        }
    }

    pub(crate) fn boot(args: Args) -> (Self, Task<Message>) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_sender, event_rx) = network::EventSender::channel();

        let session_path = session_path(&args).unwrap_or_else(|_| PathBuf::from("session"));
        let remembered = network::load_session(&session_path, Some(&args.broker));
        let preferences_path = network::preferences_path(&session_path);
        let last_server_path = network::last_server_path(&session_path);

        let tts_auto_speak = network::load_tts_auto_speak(&preferences_path);

        let initial_server = if !args.broker.trim().is_empty() {
            args.broker.clone()
        } else {
            network::load_last_server(&last_server_path).unwrap_or_default()
        };

        let net_args = args.clone();
        if !args.inspect {
            tokio::spawn(network::run(net_args, remembered, command_rx, event_sender));
        }

        let mut app = Self::new(command_tx, event_rx);
        app.fifo_path = args.inspect_control;
        app.preferences_path = Some(preferences_path);
        app.server_address = initial_server;
        app.tts_auto_speak = tts_auto_speak;
        app.admin.light_mode = false;
        app.admin.glass_mode = true;

        if args.inspect {
            app.load_inspection_demo();
            app.screen = Screen::Chat;
            app.connected = true;
        }

        (app, Task::none())
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::InspectControl(cmd) => {
                let parts: Vec<&str> = cmd.split_whitespace().collect();
                if parts.is_empty() {
                    return Task::none();
                }
                match parts[0] {
                    "resize" => {
                        if parts.len() >= 3 {
                            if let (Ok(w), Ok(h)) =
                                (parts[1].parse::<f32>(), parts[2].parse::<f32>())
                            {
                                return window::latest()
                                    .and_then(move |id| window::resize(id, Size::new(w, h)));
                            }
                        }
                    }
                    "navigation" => {
                        self.compact_sidebar_open = parts.get(1) == Some(&"open");
                    }
                    "zoom" => {
                        if parts.len() >= 2 {
                            if let Ok(factor) = parts[1].parse::<f32>() {
                                self.zoom = factor.clamp(0.5, 3.0);
                            }
                        }
                    }
                    "screenshot" => {
                        if parts.len() >= 2 {
                            self.pending_screenshot_path = Some(PathBuf::from(parts[1]));
                            return window::latest()
                                .and_then(window::screenshot)
                                .map(Message::ScreenshotCaptured);
                        }
                    }
                    "sidebar" => {
                        if parts.len() >= 2 {
                            self.sidebar_visible = match parts[1] {
                                "open" => true,
                                "closed" => false,
                                _ => !self.sidebar_visible,
                            };
                        } else {
                            self.sidebar_visible = !self.sidebar_visible;
                        }
                    }
                    "tools" => {
                        self.screen = if self.screen == Screen::Admin {
                            Screen::Chat
                        } else {
                            Screen::Admin
                        };
                    }
                    "admin" => {
                        self.screen = Screen::Admin;
                        self.admin.admin_tab =
                            parts.get(1).and_then(|tab| tab.parse().ok()).unwrap_or(0);
                    }
                    "advanced" => {
                        self.admin.models.advanced_settings = parts.get(1) == Some(&"open");
                    }
                    "theme" => {
                        self.light_mode = parts.get(1) == Some(&"light");
                        self.admin.light_mode = self.light_mode;
                    }
                    "models" => {
                        self.screen = Screen::Admin;
                        self.admin.admin_tab = 2;
                        self.admin.models.page =
                            parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
                    }
                    "settings" => {
                        self.screen = Screen::Settings;
                    }
                    "new_chat" => {
                        self.new_chat_open = true;
                    }
                    "worlds" => {
                        self.worlds_open = true;
                    }
                    "characters" => {
                        self.draft_character_open = true;
                    }
                    "memories" => {
                        self.memory_dialog_open = true;
                    }
                    "close_dialogs" => {
                        self.new_chat_open = false;
                        self.worlds_open = false;
                        self.draft_character_open = false;
                        self.memory_dialog_open = false;
                        if self.screen == Screen::Settings || self.screen == Screen::Admin {
                            self.screen = Screen::Chat;
                        }
                    }
                    "hover" => {
                        if parts.len() >= 2 {
                            if parts[1] == "none" {
                                self.hovered_message_id = None;
                                self.hovered_conversation_id = None;
                            } else if parts[1] == "conv" {
                                self.hovered_conversation_id = parts.get(2).map(|s| s.to_string());
                            } else if self.conversations.iter().any(|c| c.id == parts[1]) {
                                self.hovered_conversation_id = Some(parts[1].to_string());
                            } else {
                                self.hovered_message_id = Some(parts[1].to_string());
                            }
                        } else if let Some(m) = self.messages.last() {
                            self.hovered_message_id = Some(m.id.clone());
                        }
                    }
                    "screen" => {
                        if parts.len() >= 2 {
                            match parts[1] {
                                "login" => {
                                    self.token.clear();
                                    self.screen = Screen::Login;
                                }
                                "main" => {
                                    self.screen = Screen::Chat;
                                }
                                _ => {}
                            }
                        }
                    }
                    "quit" => {
                        return window::latest().and_then(window::close);
                    }
                    _ => {}
                }
                Task::none()
            }
            Message::ScreenshotCaptured(screenshot) => {
                if let Some(path) = self.pending_screenshot_path.take() {
                    let _ = image::save_buffer(
                        &path,
                        &screenshot.rgba,
                        screenshot.size.width,
                        screenshot.size.height,
                        image::ExtendedColorType::Rgba8,
                    );
                }
                Task::none()
            }
            Message::ServerAddressChanged(addr) => {
                self.server_address = addr;
                Task::none()
            }
            Message::TryConnect => {
                if !self.connecting {
                    if let Some(target) = connection_target(&self.server_address) {
                        self.server_address = target.server_name.clone();
                        self.connecting = true;
                        self.status =
                            format!("Connecting to {}:\u{202f}7443\u{2026}", self.server_address);
                        self.error = None;
                        let _ = self.commands.send(Command::Connect(target));
                    } else {
                        self.set_error("Enter a valid IP address or domain.");
                    }
                }
                Task::none()
            }
            Message::LoginUsernameChanged(u) => {
                self.username = u;
                Task::none()
            }
            Message::LoginPasswordChanged(p) => {
                self.password = p;
                Task::none()
            }
            Message::SubmitLogin => {
                self.send(Request::Login {
                    username: self.username.clone(),
                    password: self.password.clone(),
                });
                self.status = "Signing in\u{2026}".into();
                Task::none()
            }
            Message::SubmitRegister => {
                self.send(Request::Register {
                    username: self.username.clone(),
                    password: self.password.clone(),
                });
                self.status = "Creating account\u{2026}".into();
                Task::none()
            }
            Message::ChangeServer => {
                let _ = self.commands.send(Command::Disconnect);
                self.connected = false;
                self.restoring_session = false;
                self.status = "Enter the server IP to begin.".into();
                Task::none()
            }
            Message::SelectConversation(id) => {
                self.compact_sidebar_open = false;
                self.open_conversation(&id);
                Task::none()
            }
            Message::DeleteConversation(id) => {
                self.send(Request::DeleteEntity {
                    session_token: self.token.clone(),
                    kind: EntityKind::Conversation,
                    entity_id: id,
                });
                Task::none()
            }
            Message::HoverConversation(id) => {
                self.hovered_conversation_id = id;
                Task::none()
            }
            Message::OpenNewChat => {
                self.new_chat_open = true;
                Task::none()
            }
            Message::OpenCharactersDialog => {
                self.draft_character_open = true;
                self.draft = DraftCharacter::default();
                Task::none()
            }
            Message::CloseCharactersDialog => {
                self.draft_character_open = false;
                Task::none()
            }
            Message::OpenWorldsDialog => {
                self.worlds_open = true;
                Task::none()
            }
            Message::CloseWorldsDialog => {
                self.worlds_open = false;
                Task::none()
            }
            Message::OpenMemoriesDialog(char_id, conv_id) => {
                self.open_memory_manager(char_id, conv_id);
                Task::none()
            }
            Message::CloseMemoriesDialog => {
                self.memory_dialog_open = false;
                Task::none()
            }
            Message::OpenAdminDialog => {
                self.screen = Screen::Admin;
                self.admin.admin_tab = 0;
                self.load_admin_tab();
                Task::none()
            }
            Message::CloseAdminDialog => {
                self.screen = Screen::Chat;
                Task::none()
            }
            Message::OpenSettingsDialog => {
                self.screen = Screen::Settings;
                self.send(Request::GetAccountUsage {
                    session_token: self.token.clone(),
                });
                Task::none()
            }
            Message::CloseSettingsDialog => {
                self.screen = Screen::Chat;
                Task::none()
            }
            Message::SetTtsAutoSpeak(speak) => {
                self.tts_auto_speak = speak;
                self.save_preferences();
                Task::none()
            }
            Message::SignOut => {
                for download in self.admin.models.downloads.values() {
                    if let Some(id) = download.request_id.filter(|_| !download.finished) {
                        let _ = self.commands.send(Command::Cancel(id));
                    }
                }
                self.admin.models = admin_monitor::ModelsState::default();
                self.send(Request::Logout {
                    session_token: self.token.clone(),
                });
                let _ = self.commands.send(Command::ClearSession);
                self.worlds.clear();
                self.world_state = WorldDialogState::default();
                self.worlds_open = false;
                self.memories.clear();
                self.memory_state = MemoryDialogState::default();
                self.memory_dialog_open = false;
                self.admin.admin_log = BrokerLog::default();
                self.admin.admin_log_search.clear();
                self.token.clear();
                self.role = None;
                self.screen = Screen::Login;
                Task::none()
            }
            Message::ToggleCompactSidebar => {
                self.compact_sidebar_open = !self.compact_sidebar_open;
                Task::none()
            }
            Message::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
                Task::none()
            }
            Message::TrustCertificate(trust) => {
                if let Some(cert) = self.pending_certificate.take() {
                    if trust {
                        let _ = self.commands.send(Command::TrustCertificate(cert));
                        self.connecting = true;
                        self.status =
                            format!("Connecting to {}:\u{202f}7443\u{2026}", self.server_address);
                    } else {
                        let _ = self.commands.send(Command::RejectCertificate);
                        self.connected = false;
                        self.status = "Certificate rejected. Enter the server IP to begin.".into();
                    }
                }
                Task::none()
            }
            Message::DismissError => {
                self.error = None;
                Task::none()
            }
            Message::DismissMemoryNotice => {
                self.memory_state.status_message = None;
                Task::none()
            }
            Message::WindowResized(_) | Message::WindowClosed => Task::none(),
            Message::Network(event) => {
                self.handle_event(event);
                Task::none()
            }
            Message::Tts(event) => {
                match event {
                    TtsEvent::Speaking(busy) => {
                        if busy {
                            self.status = "Playing speech…".into();
                        }
                    }
                    TtsEvent::Status(status) => {
                        self.status = status;
                    }
                    TtsEvent::Error(err) => {
                        self.set_error(err);
                    }
                    TtsEvent::Finished => {}
                }
                Task::none()
            }
            Message::Admin(msg) => {
                if matches!(msg, AdminMessage::CloseDialog) {
                    self.screen = Screen::Chat;
                } else if matches!(msg, AdminMessage::RefreshMonitor) {
                    self.refresh_admin_monitor_if_due();
                } else if let AdminMessage::CancelDownload(model) = &msg {
                    if let Some(id) = self.admin.models.cancel_download(model) {
                        let _ = self.commands.send(Command::Cancel(id));
                    }
                } else {
                    let reqs = self.admin.update(msg, &self.token);
                    for req in reqs {
                        self.send(req);
                    }
                }
                Task::none()
            }
            Message::Character(msg) => {
                match msg {
                    CharacterMessage::CloseDialog => {
                        self.draft_character_open = false;
                    }
                    CharacterMessage::Select(id) => {
                        if let Some(c) = self.characters.iter().find(|c| c.id == id) {
                            self.draft = DraftCharacter::from(c);
                            self.draft_world_ids = self
                                .worlds
                                .iter()
                                .filter(|w| w.character_ids.contains(&id))
                                .map(|w| w.id.clone())
                                .collect();
                        }
                    }
                    CharacterMessage::New => {
                        self.draft = DraftCharacter::default();
                        self.draft_world_ids.clear();
                    }
                    CharacterMessage::Save => {
                        let input = CharacterInput::from(&self.draft);
                        self.send(Request::UpsertCharacter {
                            session_token: self.token.clone(),
                            character: Box::new(input),
                            world_ids: self.draft_world_ids.iter().cloned().collect(),
                        });
                        self.draft_character_open = false;
                    }
                    CharacterMessage::Delete => {
                        if let Some(id) = self.draft.id.clone() {
                            self.send(Request::DeleteEntity {
                                session_token: self.token.clone(),
                                kind: EntityKind::Character,
                                entity_id: id,
                            });
                            self.draft_character_open = false;
                        }
                    }
                    CharacterMessage::ToggleWorldLink { world_id, linked } => {
                        if linked {
                            self.draft_world_ids.insert(world_id);
                        } else {
                            self.draft_world_ids.remove(&world_id);
                        }
                    }
                    CharacterMessage::SearchChanged(query) => {
                        self.character_search = query;
                    }
                    CharacterMessage::Chat(id) => {
                        self.draft_character_open = false;
                        self.selected_characters.clear();
                        self.selected_characters.insert(id);
                        self.new_chat_open = true;
                    }
                    CharacterMessage::OpenMemories(id) => {
                        self.draft_character_open = false;
                        self.open_memory_manager(Some(id), None);
                    }
                    other => {
                        self.draft.update(&other);
                    }
                }
                Task::none()
            }
            Message::World(msg) => {
                let action = self.world_state.update(&self.worlds, msg);
                match action {
                    WorldAction::Save(world) => {
                        self.send(Request::SaveWorld {
                            session_token: self.token.clone(),
                            world,
                        });
                        self.worlds_open = false;
                    }
                    WorldAction::Delete(world_id) => {
                        self.send(Request::DeleteWorld {
                            session_token: self.token.clone(),
                            world_id,
                        });
                        self.worlds_open = false;
                    }
                    WorldAction::Import {
                        source_name,
                        lorebook_json,
                    } => {
                        self.send(Request::ImportSillyTavernWorld {
                            session_token: self.token.clone(),
                            source_name,
                            lorebook_json,
                        });
                    }
                    WorldAction::Close => {
                        self.worlds_open = false;
                    }
                    WorldAction::Error(err) => {
                        self.set_error(err);
                    }
                    WorldAction::None => {}
                }
                Task::none()
            }
            Message::Memory(msg) => {
                let action = self.memory_state.update(&self.memories, msg);
                match action {
                    MemoryAction::Save(input) => {
                        self.send(Request::UpsertMemory {
                            session_token: self.token.clone(),
                            memory: input,
                        });
                        self.memory_state.status_message = Some("Saving memory…".into());
                    }
                    MemoryAction::Delete(id) => {
                        self.send(Request::DeleteEntity {
                            session_token: self.token.clone(),
                            kind: EntityKind::Memory,
                            entity_id: id,
                        });
                        self.memory_state.status_message = Some("Removing memory…".into());
                    }
                    MemoryAction::ExtractFromChat => {
                        let cid = self.selected_conversation.clone();
                        let char_id = self.selected_speaker().map(str::to_owned);
                        if let Some(cid) = cid {
                            self.send(Request::ExtractMemory {
                                session_token: self.token.clone(),
                                conversation_id: cid,
                                character_id: char_id,
                            });
                            self.memory_state.status_message = Some("Learning from chat…".into());
                        }
                    }
                    MemoryAction::Condense => {
                        self.send(Request::CondenseMemories {
                            session_token: self.token.clone(),
                            conversation_id: self.memory_state.filter_conversation_id.clone(),
                            character_id: self.memory_state.filter_character_id.clone(),
                            kind: self.memory_state.filter_kind,
                        });
                        self.memory_state.status_message = Some("Condensing memories…".into());
                    }
                    MemoryAction::Refresh { .. } => {
                        self.request_filtered_memories();
                    }
                    MemoryAction::Close => {
                        self.memory_dialog_open = false;
                    }
                    MemoryAction::Error(err) => {
                        self.set_error(err);
                    }
                    MemoryAction::None => {}
                }
                Task::none()
            }
            Message::Conversation(msg) => {
                match msg {
                    ConversationMessage::InputChanged(val) => {
                        self.input = val;
                    }
                    ConversationMessage::SubmitMessage => {
                        self.submit_message();
                    }
                    ConversationMessage::CancelRequest(id) => {
                        let _ = self.commands.send(Command::Cancel(id));
                    }
                    ConversationMessage::StartEditMessage { id, content } => {
                        self.editing_message_id = Some(id);
                        self.editing_message_content = content;
                    }
                    ConversationMessage::EditContentChanged(val) => {
                        self.editing_message_content = val;
                    }
                    ConversationMessage::SaveMessageEdit => {
                        if let Some(id) = self.editing_message_id.take() {
                            let content = std::mem::take(&mut self.editing_message_content);
                            self.send(Request::UpdateMessage {
                                session_token: self.token.clone(),
                                message_id: id,
                                content,
                            });
                        }
                    }
                    ConversationMessage::CancelMessageEdit => {
                        self.editing_message_id = None;
                        self.editing_message_content.clear();
                    }
                    ConversationMessage::DeleteMessage(id) => {
                        self.send(Request::DeleteEntity {
                            session_token: self.token.clone(),
                            kind: EntityKind::Message,
                            entity_id: id,
                        });
                    }
                    ConversationMessage::RegenerateMessage(id) => {
                        if let Some(message) = self.messages.iter().find(|m| m.id == id).cloned() {
                            self.regenerate(&message);
                        }
                    }
                    ConversationMessage::SpeakMessage { text, author_id } => {
                        if let Err(e) = self.tts.speak(&text, author_id.as_deref()) {
                            self.set_error(e);
                        }
                    }
                    ConversationMessage::SelectVariant {
                        message_id,
                        variant_id,
                    } => {
                        self.send(Request::SelectVariant {
                            session_token: self.token.clone(),
                            message_id,
                            variant_id,
                        });
                    }
                    ConversationMessage::OpenNewChatDialog => {
                        self.new_chat_open = true;
                    }
                    ConversationMessage::CloseNewChatDialog => {
                        self.new_chat_open = false;
                    }
                    ConversationMessage::NewChatTitleChanged(title) => {
                        self.new_conversation_title = title;
                    }
                    ConversationMessage::SelectNewChatCharacter(id) => {
                        self.selected_characters.clear();
                        self.selected_characters.insert(id);
                    }
                    ConversationMessage::CreateNewChat => {
                        let participant_ids: Vec<String> =
                            self.selected_characters.iter().cloned().collect();
                        let title = if self.new_conversation_title.trim().is_empty() {
                            "New Conversation".to_string()
                        } else {
                            self.new_conversation_title.trim().to_string()
                        };
                        self.send(Request::CreateConversation {
                            session_token: self.token.clone(),
                            title,
                            kind: ConversationKind::Direct,
                            participant_ids,
                        });
                        self.new_chat_open = false;
                        self.compact_sidebar_open = false;
                        self.new_conversation_title.clear();
                    }
                    ConversationMessage::ToggleSidebar => {
                        self.sidebar_visible = !self.sidebar_visible;
                    }
                    ConversationMessage::OpenLink(_) => {}
                    ConversationMessage::HoverMessage(id) => {
                        self.hovered_message_id = id;
                    }
                }
                Task::none()
            }
        }
    }

    pub(crate) fn view<'a>(&'a self) -> Element<'a, Message> {
        responsive(move |size| self.view_at_size(size)).into()
    }

    fn view_at_size<'a>(&'a self, size: Size) -> Element<'a, Message> {
        let base: Element<Message> = if !self.connected {
            self.server_connection_view()
        } else if self.restoring_session {
            self.session_restore_view()
        } else if self.token.is_empty() {
            self.login_view()
        } else {
            self.shell_view(size)
        };

        let scrim_color = if self.glass_mode {
            if self.light_mode {
                Color::from_rgba(0.0, 0.0, 0.0, 0.32)
            } else {
                Color::from_rgba(0.0, 0.0, 0.0, 130.0 / 255.0)
            }
        } else {
            Color::from_rgba(0.0, 0.0, 0.0, 0.35)
        };

        // Keep the base wrapped in the dialog stack even when no modal is open. Returning the
        // bare base would change the root widget type whenever a modal opens or closes, which
        // discards the widget tree state and resets every scroll offset to the top.
        let root: Element<Message> = if let Some(cert) = &self.pending_certificate {
            dialog(
                true,
                base,
                scrollable(self.certificate_trust_dialog_view(cert)),
            )
            .backdrop(scrim_color)
            .padding_inner(12)
            .padding_outer(8)
            .max_height((size.height - 16.0).max(1.0))
            .max_width(460.0)
            .on_press(Message::TrustCertificate(false))
            .container_style(move |_| {
                ui::modal_dialog_frame_style(self.light_mode, self.glass_mode)
            })
            .into()
        } else if let Some(err) = &self.error {
            dialog(true, base, scrollable(self.error_modal_view(err)))
                .backdrop(scrim_color)
                .padding_inner(12)
                .padding_outer(8)
                .max_height((size.height - 16.0).max(1.0))
                .max_width(400.0)
                .on_press(Message::DismissError)
                .container_style(move |_| {
                    ui::modal_dialog_frame_style(self.light_mode, self.glass_mode)
                })
                .into()
        } else if self.worlds_open {
            let world_state = WorldViewState {
                worlds: &self.worlds,
                world_draft: &self.world_state.world_draft,
                characters: &self.characters,
                entry_search: &self.world_state.entry_search,
                import_pending: self.world_state.import_pending,
                import_elapsed_secs: self
                    .world_state
                    .import_started
                    .map(|s| s.elapsed().as_secs())
                    .unwrap_or(0),
                import_notice: self.world_state.import_notice.as_deref(),
                light_mode: self.light_mode,
                glass_mode: self.glass_mode,
            };
            dialog(true, base, world_dialog_view(&world_state))
                .backdrop(scrim_color)
                .padding_inner(12)
                .padding_outer(8)
                .max_width(740.0)
                .max_height((size.height - 32.0).clamp(1.0, 780.0))
                .on_press(Message::CloseWorldsDialog)
                .container_style(move |_| {
                    ui::modal_dialog_frame_style(self.light_mode, self.glass_mode)
                })
                .into()
        } else if self.memory_dialog_open {
            let can_extract =
                self.selected_conversation.is_some() && self.selected_speaker().is_some();
            let memory_view_state = MemoryViewState::new(
                &self.memories,
                &self.characters,
                &self.conversations,
                &self.memory_state,
                can_extract,
                self.light_mode,
                self.glass_mode,
            );
            dialog(true, base, memory_dialog_view(&memory_view_state))
                .backdrop(scrim_color)
                .padding_inner(12)
                .padding_outer(8)
                .max_width(780.0)
                .max_height((size.height - 32.0).clamp(1.0, 780.0))
                .on_press(Message::CloseMemoriesDialog)
                .container_style(move |_| {
                    ui::modal_dialog_frame_style(self.light_mode, self.glass_mode)
                })
                .into()
        } else if self.draft_character_open {
            let char_view = character_dialog_view(
                &self.characters,
                &self.draft,
                &self.draft_world_ids,
                &self.worlds,
                &self.character_search,
                self.char_import_pending,
                self.light_mode,
                self.glass_mode,
                size.width < 800.0,
            )
            .map(Message::Character);
            dialog(true, base, char_view)
                .backdrop(scrim_color)
                .padding_inner(12)
                .padding_outer(8)
                .max_width(940.0)
                .max_height((size.height - 32.0).clamp(1.0, 740.0))
                .on_press(Message::CloseCharactersDialog)
                .container_style(move |_| {
                    ui::modal_dialog_frame_style(self.light_mode, self.glass_mode)
                })
                .into()
        } else if self.new_chat_open {
            let new_chat_state = NewChatViewState {
                is_open: self.new_chat_open,
                characters: &self.characters,
                selected_character_id: self.selected_characters.iter().next().map(|s| s.as_str()),
                title: &self.new_conversation_title,
                light_mode: self.light_mode,
                glass_mode: self.glass_mode,
            };
            if let Some(content) = new_chat_dialog_view(&new_chat_state, Message::Conversation) {
                dialog(true, base, content)
                    .backdrop(scrim_color)
                    .padding_inner(12)
                    .padding_outer(8)
                    .max_width(460.0)
                    .max_height((size.height - 32.0).clamp(1.0, 580.0))
                    .on_press(Message::Conversation(
                        ConversationMessage::CloseNewChatDialog,
                    ))
                    .container_style(move |_| {
                        ui::modal_dialog_frame_style(self.light_mode, self.glass_mode)
                    })
                    .into()
            } else {
                dialog(false, base, Space::new()).into()
            }
        } else if self.screen == Screen::Admin {
            let admin_view_state = self.admin.as_view_state();
            dialog(true, base, admin_dialog_view(&admin_view_state))
                .backdrop(scrim_color)
                .padding_inner(12)
                .padding_outer(8)
                .max_width(880.0)
                .max_height((size.height - 32.0).clamp(1.0, 740.0))
                .on_press(Message::CloseAdminDialog)
                .container_style(move |_| {
                    let mut style = ui::modal_dialog_frame_style(self.light_mode, self.glass_mode);
                    style.background = Some(Background::Color(if self.light_mode {
                        Color::WHITE
                    } else {
                        ui::DARK_PALETTE.background
                    }));
                    style
                })
                .into()
        } else if self.screen == Screen::Settings {
            dialog(true, base, scrollable(self.settings_dialog_view()))
                .backdrop(scrim_color)
                .padding_inner(12)
                .padding_outer(8)
                .max_width(440.0)
                .max_height((size.height - 32.0).clamp(1.0, 600.0))
                .on_press(Message::CloseSettingsDialog)
                .container_style(move |_| {
                    ui::modal_dialog_frame_style(self.light_mode, self.glass_mode)
                })
                .into()
        } else {
            dialog(false, base, Space::new()).into()
        };

        let light_mode = self.light_mode;
        let glass_mode = self.glass_mode;

        container(root)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| ui::window_backdrop_style(light_mode, glass_mode))
            .into()
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        let net_sub = Subscription::run_with(self.events_holder.clone(), run_network_stream)
            .map(Message::Network);
        let tts_sub =
            Subscription::run_with(self.tts_holder.clone(), run_tts_stream).map(Message::Tts);
        let admin_sub =
            admin_subscription(self.screen == Screen::Admin && self.admin.admin_tab == 0)
                .map(Message::Admin);

        let fifo_sub = if let Some(path) = &self.fifo_path {
            Subscription::run_with(path.clone(), run_fifo_stream).map(Message::InspectControl)
        } else {
            Subscription::none()
        };

        Subscription::batch(vec![net_sub, tts_sub, admin_sub, fifo_sub])
    }

    pub(crate) fn theme(&self) -> Theme {
        ui::theme(self.light_mode)
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        self.zoom
    }

    fn server_connection_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Connect to Chatty").size(28).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        });
        let subtitle = text("Enter the IP address or domain of your Chatty server.")
            .size(14)
            .color(ui::secondary_text_color(self.light_mode));

        let addr_input = text_input("192.168.0.98 or chatty.example.com", &self.server_address)
            .on_input(Message::ServerAddressChanged)
            .on_submit(Message::TryConnect)
            .padding(10)
            .width(Length::Fill);

        let connect_btn = button(
            text(if self.connecting {
                "Trying connection…"
            } else {
                "Try connection"
            })
            .size(15)
            .font(font::Font {
                weight: font::Weight::Bold,
                ..Default::default()
            }),
        )
        .on_press(Message::TryConnect)
        .padding(10)
        .width(Length::Fill)
        .style(ui::primary_button_style());

        let status_txt = text(&self.status)
            .size(13)
            .color(ui::secondary_text_color(self.light_mode));

        let card_content = column![
            title,
            subtitle,
            Space::new().height(16),
            text("Server address").size(13),
            addr_input,
            Space::new().height(8),
            connect_btn,
            Space::new().height(6),
            status_txt,
        ]
        .spacing(6)
        .max_width(420)
        .align_x(Alignment::Center);

        container(scrollable(
            container(ui::card(card_content, self.light_mode, self.glass_mode).max_width(460))
                .padding(16)
                .center_x(Length::Fill),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }

    fn session_restore_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Signing you in").size(28).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        });
        let status_txt = text(&self.status).size(14);

        let card_content = column![title, Space::new().height(12), status_txt]
            .spacing(8)
            .align_x(Alignment::Center);

        container(scrollable(
            container(ui::card(card_content, self.light_mode, self.glass_mode).max_width(460))
                .padding(16)
                .center_x(Length::Fill),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }

    fn login_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Welcome back").size(28).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        });
        let status_txt = text(&self.status)
            .size(13)
            .color(ui::secondary_text_color(self.light_mode));

        let username_input = text_input("Username", &self.username)
            .on_input(Message::LoginUsernameChanged)
            .on_submit(Message::SubmitLogin)
            .padding(10)
            .width(Length::Fill);

        let password_input = text_input("Password", &self.password)
            .on_input(Message::LoginPasswordChanged)
            .on_submit(Message::SubmitLogin)
            .secure(true)
            .padding(10)
            .width(Length::Fill);

        let sign_in_btn = button(text("Sign in").size(15).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        }))
        .on_press(Message::SubmitLogin)
        .padding(10)
        .width(Length::Fill)
        .style(ui::primary_button_style());

        let mut card_col = column![
            title,
            status_txt,
            Space::new().height(14),
            text("Username").size(13),
            username_input,
            text("Password").size(13),
            password_input,
            Space::new().height(6),
            sign_in_btn,
        ]
        .spacing(6)
        .max_width(420)
        .align_x(Alignment::Center);

        if self.registration_enabled {
            let register_btn = button(text("Register").size(14))
                .on_press(Message::SubmitRegister)
                .padding(8)
                .width(Length::Fill)
                .style(ui::secondary_button_style(self.light_mode));
            card_col = card_col.push(register_btn);
        }

        let change_server_btn = button(text("Change server").size(13))
            .on_press(Message::ChangeServer)
            .padding(6)
            .style(ui::secondary_button_style(self.light_mode));
        card_col = card_col
            .push(Space::new().height(6))
            .push(change_server_btn);

        container(scrollable(
            container(ui::card(card_col, self.light_mode, self.glass_mode).max_width(460))
                .padding(16)
                .center_x(Length::Fill),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }

    fn sidebar_view<'a>(&'a self, compact: bool, short: bool) -> Element<'a, Message> {
        let header = row![
            text("Chatty").size(22).font(font::Font {
                weight: font::Weight::Bold,
                ..Default::default()
            }),
            Space::new().width(Length::Fill),
            sidebar_toggle_button(
                true,
                if compact {
                    Message::ToggleCompactSidebar
                } else {
                    Message::ToggleSidebar
                }
            ),
        ]
        .align_y(Alignment::Center);

        let new_chat_btn = button(
            row![
                text("+").size(18),
                text("New chat").size(15).font(font::Font {
                    weight: font::Weight::Bold,
                    ..Default::default()
                })
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .on_press(Message::OpenNewChat)
        .padding(10)
        .width(Length::Fill)
        .style(ui::primary_button_style());

        let recent_label = text("RECENT CHATS")
            .size(11)
            .color(ui::secondary_text_color(self.light_mode));

        let mut conv_col = column![].spacing(4).width(Length::Fill);
        for conv in &self.conversations {
            let is_selected = self.selected_conversation.as_deref() == Some(&conv.id);
            let is_hovered = self.hovered_conversation_id.as_deref() == Some(&conv.id);
            let conv_id = conv.id.clone();
            let conv_del_id = conv.id.clone();
            let conv_hover_id = conv.id.clone();

            let title_btn = button(
                container(text(&conv.title).size(14).wrapping(text::Wrapping::None))
                    .align_y(Alignment::Center)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(Message::SelectConversation(conv_id))
            .height(Length::Fixed(38.0))
            .padding(Padding::from([0, 10]))
            .width(Length::Fill)
            .style(move |theme: &Theme, status: button::Status| {
                if is_selected {
                    button::Style {
                        background: Some(Background::Color(ui::COLOR_PRIMARY_STRONG)),
                        text_color: Color::WHITE,
                        border: Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: 8.0.into(),
                        },
                        ..Default::default()
                    }
                } else {
                    let bg = match status {
                        button::Status::Hovered => Color::from_rgba(
                            theme.palette().text.r,
                            theme.palette().text.g,
                            theme.palette().text.b,
                            0.08,
                        ),
                        button::Status::Pressed => Color::from_rgba(
                            theme.palette().text.r,
                            theme.palette().text.g,
                            theme.palette().text.b,
                            0.14,
                        ),
                        _ => Color::TRANSPARENT,
                    };
                    button::Style {
                        background: if bg == Color::TRANSPARENT {
                            None
                        } else {
                            Some(Background::Color(bg))
                        },
                        text_color: theme.palette().text,
                        border: Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: 8.0.into(),
                        },
                        ..Default::default()
                    }
                }
            });

            let tile_item: Element<'_, Message> = if is_hovered {
                let tile_bg = if is_selected {
                    ui::COLOR_PRIMARY_STRONG
                } else {
                    let base = ui::surface_color(self.light_mode, self.glass_mode);
                    let overlay = if self.light_mode {
                        Color::BLACK
                    } else {
                        Color::WHITE
                    };
                    Color::from_rgb(
                        base.r * 0.92 + overlay.r * 0.08,
                        base.g * 0.92 + overlay.g * 0.08,
                        base.b * 0.92 + overlay.b * 0.08,
                    )
                };

                let delete_btn = button(
                    container(text("×").size(16))
                        .center_x(Length::Fill)
                        .center_y(Length::Fill),
                )
                .on_press(Message::DeleteConversation(conv_del_id))
                .width(26)
                .height(26)
                .padding(0)
                .style(move |_theme: &Theme, status: button::Status| {
                    let (bg, border_color, text_color) = match status {
                        button::Status::Hovered => (
                            Color::from_rgba(0.95, 0.2, 0.2, 0.25),
                            Color::from_rgba(0.95, 0.2, 0.2, 0.50),
                            Color::from_rgb(1.0, 0.35, 0.35),
                        ),
                        button::Status::Pressed => (
                            Color::from_rgba(0.95, 0.2, 0.2, 0.40),
                            Color::from_rgba(0.95, 0.2, 0.2, 0.70),
                            Color::from_rgb(1.0, 0.45, 0.45),
                        ),
                        _ => (
                            Color::TRANSPARENT,
                            Color::TRANSPARENT,
                            if is_selected {
                                Color::from_rgba(1.0, 1.0, 1.0, 0.85)
                            } else {
                                Color::from_rgba(0.95, 0.35, 0.35, 0.85)
                            },
                        ),
                    };
                    button::Style {
                        background: if bg == Color::TRANSPARENT {
                            None
                        } else {
                            Some(Background::Color(bg))
                        },
                        text_color,
                        border: Border {
                            color: border_color,
                            width: 0.0,
                            radius: 6.0.into(),
                        },
                        ..Default::default()
                    }
                });

                let delete_tooltip = tooltip(
                    delete_btn,
                    container(text("Delete chat").size(12))
                        .padding(Padding::from([4, 8]))
                        .style(ui::tooltip_style),
                    tooltip::Position::Top,
                );

                let fade_box = container(
                    row![
                        Space::new().width(Length::Fill),
                        delete_tooltip,
                        Space::new().width(6),
                    ]
                    .align_y(Alignment::Center)
                    .height(Length::Fill),
                )
                .width(68)
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(tile_bg)),
                    border: Border {
                        radius: iced::border::Radius {
                            top_left: 0.0,
                            top_right: 8.0,
                            bottom_right: 8.0,
                            bottom_left: 0.0,
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                });

                let overlay = row![Space::new().width(Length::Fill), fade_box,]
                    .width(Length::Fill)
                    .height(Length::Fill);

                let tile_stack = stack![title_btn, overlay,];

                mouse_area(tile_stack)
                    .on_enter(Message::HoverConversation(Some(conv_hover_id)))
                    .on_exit(Message::HoverConversation(None))
                    .into()
            } else {
                let tile_stack = stack![title_btn,];

                mouse_area(tile_stack)
                    .on_enter(Message::HoverConversation(Some(conv_hover_id)))
                    .on_exit(Message::HoverConversation(None))
                    .into()
            };

            conv_col = conv_col.push(tile_item);
        }

        let conv_list = scrollable(conv_col)
            .id(Id::new("conversation-list"))
            .height(if short { Length::Shrink } else { Length::Fill })
            .width(Length::Fill);

        let status_row = row![
            text(&self.status)
                .size(12)
                .color(ui::secondary_text_color(self.light_mode))
        ];

        let mut icons_row = row![
            icon_button(
                FooterIcon::Characters,
                Some("Manage characters"),
                Message::OpenCharactersDialog,
            ),
            icon_button(
                FooterIcon::World,
                Some("Manage world lore"),
                Message::OpenWorldsDialog,
            ),
            icon_button(
                FooterIcon::Memory,
                Some("Manage character memories"),
                Message::OpenMemoriesDialog(None, None),
            ),
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        if self.role == Some(Role::Admin) {
            icons_row = icons_row.push(icon_button(
                FooterIcon::Admin,
                Some("Open admin portal"),
                Message::OpenAdminDialog,
            ));
        }

        icons_row = icons_row
            .push(icon_button(
                FooterIcon::Settings,
                Some("Open settings"),
                Message::OpenSettingsDialog,
            ))
            .push(icon_button(
                FooterIcon::SignOut,
                Some("Sign out"),
                Message::SignOut,
            ));

        let sidebar_content = column![
            header,
            Space::new().height(8),
            new_chat_btn,
            Space::new().height(10),
            recent_label,
            Space::new().height(4),
            conv_list,
            Space::new().height(8),
            status_row,
            Space::new().height(6),
            icons_row.wrap(),
        ]
        .spacing(4)
        .padding(12)
        .width(if compact {
            Length::Fill
        } else {
            Length::Fixed(248.0)
        })
        .height(Length::Fill);

        let sidebar_content: Element<'a, Message> = if short {
            scrollable(sidebar_content.height(Length::Shrink)).into()
        } else {
            sidebar_content.into()
        };
        sidebar_panel(sidebar_content, self.light_mode, self.glass_mode)
            .height(Length::Fill)
            .into()
    }

    fn shell_view<'a>(&'a self, size: Size) -> Element<'a, Message> {
        let compact = size.width < 800.0 || size.height < 360.0;
        if compact && self.compact_sidebar_open {
            return self.sidebar_view(true, size.height < 400.0);
        }
        let sidebar_visible = !compact && self.sidebar_visible;
        let mut shell_row = row![].width(Length::Fill).height(Length::Fill);

        if sidebar_visible {
            shell_row = shell_row.push(self.sidebar_view(false, false));
        }

        let conv_state = ConversationViewState {
            characters: &self.characters,
            conversations: &self.conversations,
            messages: &self.messages,
            selected_conversation: self.selected_conversation.as_deref(),
            selected_speaker: self.selected_speaker(),
            input: &self.input,
            active_request: self.active_request,
            stream_text: &self.stream_text,
            typing_character: self.typing_character.as_deref(),
            generation_status: self.generation_status,
            editing_message_id: self.editing_message_id.as_deref(),
            editing_message_content: &self.editing_message_content,
            hovered_message_id: self.hovered_message_id.as_deref(),
            sidebar_visible,
            available_size: Size::new(
                size.width - if sidebar_visible { 272.0 } else { 0.0 },
                size.height,
            ),
            light_mode: self.light_mode,
            glass_mode: self.glass_mode,
        };

        let chat_area = conversation_view(&conv_state, move |message| {
            if compact && matches!(message, ConversationMessage::ToggleSidebar) {
                Message::ToggleCompactSidebar
            } else {
                Message::Conversation(message)
            }
        });

        shell_row = shell_row.push(chat_area);

        container(shell_row)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn settings_dialog_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Settings").size(22).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        });

        let speech_header = text("Local Speech (TTS)").size(14).font(font::Font {
            weight: font::Weight::Semibold,
            ..Default::default()
        });
        let auto_speak_toggle = toggler(self.tts_auto_speak)
            .label("Automatically read new replies")
            .on_toggle(Message::SetTtsAutoSpeak);

        let test_voice_btn = button(text("Test voice").size(13))
            .on_press(Message::Conversation(ConversationMessage::SpeakMessage {
                text: "Chatty local speech synthesis is working properly.".into(),
                author_id: None,
            }))
            .padding([6, 12])
            .style(ui::secondary_button_style(self.light_mode));

        let usage_header = text("Account Token Usage").size(14).font(font::Font {
            weight: font::Weight::Semibold,
            ..Default::default()
        });
        let usage_txt = text(format!(
            "Prompt: {} · Completion: {} · Total: {}",
            self.account_usage.prompt_tokens,
            self.account_usage.completion_tokens,
            self.account_usage.prompt_tokens + self.account_usage.completion_tokens
        ))
        .size(13);

        let close_btn = button(text("Close").size(14).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        }))
        .on_press(Message::CloseSettingsDialog)
        .padding([8, 16])
        .style(ui::primary_button_style());

        let content = column![
            title,
            Space::new().height(10),
            speech_header,
            auto_speak_toggle,
            test_voice_btn,
            Space::new().height(12),
            usage_header,
            usage_txt,
            Space::new().height(16),
            close_btn,
        ]
        .spacing(6)
        .padding(20)
        .max_width(400);

        container(content).width(Length::Fill).into()
    }

    fn certificate_trust_dialog_view<'a>(
        &'a self,
        cert: &'a PendingCertificate,
    ) -> Element<'a, Message> {
        let title = text("Trust this Chatty server?").size(20).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        });

        let desc = text(format!(
            "{} does not have a certificate trusted by the public internet.",
            cert.target.server_name
        ))
        .size(13);

        let subject = if cert.is_ca() {
            "private CA"
        } else {
            "server certificate"
        };

        let fp_desc = text(format!(
            "Confirm this {subject} SHA-256 fingerprint with the server administrator before continuing:"
        ))
        .size(12);

        let fp_text = text(&cert.fingerprint).size(11).font(font::Font::MONOSPACE);

        let trust_btn = button(text("Trust and connect").size(14).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        }))
        .on_press(Message::TrustCertificate(true))
        .padding(10)
        .style(ui::primary_button_style());

        let cancel_btn = button(text("Cancel").size(14))
            .on_press(Message::TrustCertificate(false))
            .padding(10)
            .style(ui::secondary_button_style(self.light_mode));

        let btn_row = row![trust_btn, cancel_btn].spacing(12);

        let content = column![
            title,
            desc,
            Space::new().height(6),
            fp_desc,
            fp_text,
            Space::new().height(12),
            btn_row,
        ]
        .spacing(6)
        .padding(20)
        .max_width(460);

        container(content).width(Length::Fill).into()
    }

    fn error_modal_view<'a>(&'a self, err: &'a UiNotice) -> Element<'a, Message> {
        let title = text("Notice").size(18).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        });
        let time_txt = text(&err.timestamp)
            .size(11)
            .color(ui::secondary_text_color(self.light_mode));
        let msg_txt = text(&err.message).size(13);

        let close_btn = button(text("Close").size(13))
            .on_press(Message::DismissError)
            .padding([6, 14])
            .style(ui::secondary_button_style(self.light_mode));

        let content = column![
            title,
            time_txt,
            Space::new().height(4),
            msg_txt,
            Space::new().height(10),
            close_btn,
        ]
        .spacing(4)
        .padding(16)
        .max_width(400);

        container(content).padding(16).width(Length::Fill).into()
    }

    pub(crate) fn send(&self, request: Request) {
        let _ = self.commands.send(Command::Request(Box::new(request)));
    }

    pub(crate) fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(UiNotice {
            timestamp: current_utc_timestamp(),
            message: message.into(),
        });
    }

    pub(crate) fn set_network_status(&mut self, status: String) {
        if status.contains(" · Offline:") {
            self.status = "Offline".into();
            self.status_tooltip = Some(status);
        } else if status.contains(" · Startup failed:") {
            self.status = "Startup failed".into();
            self.status_tooltip = Some(status);
        } else {
            self.status = status;
            self.status_tooltip = None;
        }
    }

    pub(crate) fn refresh(&self) {
        self.send(Request::ListCharacters {
            session_token: self.token.clone(),
        });
        self.send(Request::ListConversations {
            session_token: self.token.clone(),
        });
        self.send(Request::ListWorlds {
            session_token: self.token.clone(),
        });
        self.send(Request::ListMemories {
            session_token: self.token.clone(),
            conversation_id: None,
            character_id: None,
        });
    }

    pub(crate) fn authenticated(
        &mut self,
        token: String,
        user_id: String,
        role: Role,
        revision: i64,
    ) {
        self.connected = true;
        self.connecting = false;
        self.restoring_session = false;
        self.token = token;
        self.user_id = user_id.clone();
        self.admin.current_user_id = user_id;
        self.role = Some(role);
        self.revision = revision;
        self.auto_select_conversation = true;
        self.status = "Online · TLS 1.3".into();
        self.status_tooltip = None;
        self.password.clear();
        self.refresh();
        self.send(Request::GetAccountUsage {
            session_token: self.token.clone(),
        });
    }

    pub(crate) fn handle_event(&mut self, event: Event) {
        match event {
            Event::DownloadStarted { request_id, model } => {
                if self.admin.models.download_started(&model, request_id) {
                    let _ = self.commands.send(Command::Cancel(request_id));
                }
            }
            Event::ModelRequestStarted(id) => {
                self.admin.models.request_id = Some(id);
                self.admin.models.busy = true;
            }
            Event::Status(status) => {
                if status.contains("Offline:") || status.contains("Idle · offline:") {
                    self.memory_state.condensing = false;
                    self.admin.models.interrupt_downloads();
                    self.admin.models.busy = false;
                    self.admin.models.request_id = None;
                    self.admin.models.notice = Some("Connection interrupted. Refresh to check whether the operation finished on the server.".into());
                }
                self.set_network_status(status);
            }
            Event::Connected { resuming_session } => {
                self.connected = true;
                self.connecting = false;
                self.restoring_session = resuming_session;
            }
            Event::ConnectionFailed(message) => {
                self.memory_state.condensing = false;
                self.admin.models.interrupt_downloads();
                self.admin.models.busy = false;
                self.admin.models.request_id = None;
                self.admin.models.notice =
                    Some("Connection failed. Try again after reconnecting.".into());
                self.pending_certificate = None;
                self.connected = false;
                self.connecting = false;
                self.restoring_session = false;
                self.active_request = None;
                self.typing_character = None;
                self.generation_status = None;
                self.status = "Not connected".into();
                self.status_tooltip = None;
                self.set_error(message);
            }
            Event::CertificateTrustRequired(certificate) => {
                self.connecting = false;
                self.status = "Waiting for certificate confirmation".into();
                self.pending_certificate = Some(certificate);
            }
            Event::Disconnected => {
                self.admin.models.interrupt_downloads();
                self.admin.models.busy = false;
                self.admin.models.request_id = None;
                self.admin.models.notice =
                    Some("Disconnected. Refresh models after reconnecting.".into());
                self.pending_certificate = None;
                self.connected = false;
                self.connecting = false;
                self.restoring_session = false;
                self.worlds.clear();
                self.world_state = WorldDialogState::default();
                self.worlds_open = false;
                self.memories.clear();
                self.memory_state = MemoryDialogState::default();
                self.memory_dialog_open = false;
                self.admin.admin_log = BrokerLog::default();
                self.admin.admin_log_search.clear();
                self.char_import_pending = false;
                self.char_import_started = None;
                self.active_request = None;
                self.typing_character = None;
                self.token.clear();
                self.character_image_requests.clear();
                self.received_character_images.clear();
                self.generation_status = None;
                self.role = None;
                self.status = "Enter the server IP to begin.".into();
                self.status_tooltip = None;
            }
            Event::SessionExpired => {
                for download in self.admin.models.downloads.values() {
                    if let Some(id) = download.request_id.filter(|_| !download.finished) {
                        let _ = self.commands.send(Command::Cancel(id));
                    }
                }
                self.admin.models = admin_monitor::ModelsState::default();
                self.restoring_session = false;
                self.worlds.clear();
                self.world_state = WorldDialogState::default();
                self.worlds_open = false;
                self.memories.clear();
                self.memory_state = MemoryDialogState::default();
                self.memory_dialog_open = false;
                self.admin.admin_log = BrokerLog::default();
                self.admin.admin_log_search.clear();
                self.char_import_pending = false;
                self.char_import_started = None;
                self.active_request = None;
                self.typing_character = None;
                self.token.clear();
                self.character_image_requests.clear();
                self.received_character_images.clear();
                self.generation_status = None;
                self.role = None;
                self.set_error("Saved session expired. Sign in again.");
            }
            Event::Frame(frame) => self.handle_frame(frame),
        }
    }

    pub(crate) fn handle_frame(&mut self, frame: Frame) {
        if frame.message_type == MessageType::Error
            && let Some(download) = self.admin.models.download_for_request(frame.request_id)
        {
            download.finished = true;
            download.request_id = None;
            download.progress.status = decode::<WireError>(&frame.payload)
                .map(|error| error.message)
                .unwrap_or_else(|_| "Download failed.".into());
            return;
        }
        if self.admin.models.request_id == Some(frame.request_id)
            && matches!(
                frame.message_type,
                MessageType::Response | MessageType::Error
            )
        {
            self.admin.models.busy = false;
            self.admin.models.request_id = None;
            self.admin.models.notice = if frame.message_type == MessageType::Error {
                Some(
                    decode::<WireError>(&frame.payload)
                        .map(|error| error.message)
                        .unwrap_or_else(|_| "Model request failed.".into()),
                )
            } else {
                None
            };
        }
        match frame.message_type {
            MessageType::Response => {
                if let Ok(response) = decode::<Response>(&frame.payload) {
                    match response {
                        Response::Authenticated {
                            session_token,
                            user_id,
                            role,
                            revision,
                        } => self.authenticated(session_token, user_id, role, revision),
                        Response::ServerCapabilities {
                            registration_enabled,
                        } => self.registration_enabled = registration_enabled,
                        Response::Worlds(v) => {
                            self.draft_world_ids
                                .retain(|id| v.iter().any(|world| &world.id == id));
                            self.worlds = v;
                        }
                        Response::WorldImportPreview(world) => {
                            self.world_state.world_draft = world;
                            self.world_state.import_pending = false;
                            self.world_state.import_started = None;
                            self.world_state.import_notice = Some(
                                "AI import preview — review the entries, link characters, then save."
                                    .into(),
                            );
                            self.worlds_open = true;
                        }
                        Response::CharacterImportPreview(character) => {
                            self.draft = DraftCharacter::from(character.as_ref());
                            self.draft.owned_by_user = true;
                            self.char_import_pending = false;
                            self.char_import_started = None;
                            self.draft_character_open = true;
                        }
                        Response::Characters(v) => {
                            self.character_image_requests.clear();
                            self.characters = v;
                        }
                        Response::Memories(v) => self.memories = v,
                        Response::MemoriesCondensed { before, after } => {
                            self.memory_state.condensing = false;
                            self.memory_state.status_message = Some(if before == 0 {
                                "No redundant memories found.".into()
                            } else {
                                format!("Condensed {before} memories into {after}.")
                            });
                            if !self.token.is_empty() {
                                self.send(Request::GetAccountUsage {
                                    session_token: self.token.clone(),
                                });
                            }
                        }
                        Response::CharacterImage {
                            character_id,
                            image_id,
                            data,
                        } => {
                            self.character_image_requests
                                .remove(&(character_id, image_id.clone()));
                            self.received_character_images.push((image_id, data));
                        }
                        Response::Conversations(v) => {
                            self.conversations = v
                                .into_iter()
                                .filter(|conversation| {
                                    conversation.kind == ConversationKind::Direct
                                })
                                .collect();
                            if self.selected_conversation.as_ref().is_some_and(|selected| {
                                !self
                                    .conversations
                                    .iter()
                                    .any(|conversation| &conversation.id == selected)
                            }) {
                                self.selected_conversation = None;
                                self.messages.clear();
                                self.stream_text.clear();
                                self.active_request = None;
                                self.typing_character = None;
                                self.generation_status = None;
                            }
                            if self.selected_conversation.is_none() && self.auto_select_conversation
                            {
                                self.selected_conversation =
                                    self.conversations.first().map(|c| c.id.clone());
                                self.messages.clear();
                                self.stream_text.clear();
                                if let Some(id) = self.selected_conversation.clone() {
                                    self.open_conversation(&id);
                                }
                            }
                        }
                        Response::ConversationView(v) => {
                            self.selected_conversation = Some(v.conversation.id);
                            self.messages = v.messages;
                            self.stream_text.clear();
                            if self.tts_auto_speak
                                && let Some(message_id) = self.pending_tts_message_id.take()
                                && let Some(message) = self
                                    .messages
                                    .iter()
                                    .find(|message| message.id == message_id)
                            {
                                let text = Self::displayed_message_content(message).to_owned();
                                if let Err(error) =
                                    self.tts.speak(&text, message.author_id.as_deref())
                                {
                                    self.set_error(error);
                                }
                            }
                        }
                        Response::ConversationNotFound { conversation_id } => {
                            if self.selected_conversation.as_deref()
                                == Some(conversation_id.as_str())
                            {
                                self.selected_conversation = None;
                                self.messages.clear();
                                self.stream_text.clear();
                                self.active_request = None;
                                self.typing_character = None;
                                self.generation_status = None;
                                self.auto_select_conversation = false;
                                self.refresh();
                            }
                        }
                        Response::ModelDownloadProgress(progress) => {
                            if let Some(download) =
                                self.admin.models.download_for_request(frame.request_id)
                            {
                                download.progress = progress;
                            }
                        }
                        Response::ModelDownloadFinished { cancelled } => {
                            if let Some(download) =
                                self.admin.models.download_for_request(frame.request_id)
                            {
                                download.finished = true;
                                download.request_id = None;
                                download.progress.status = if cancelled {
                                    "Cancelled"
                                } else {
                                    "Download complete"
                                }
                                .into();
                                if !cancelled {
                                    self.send(Request::AdminGetOllamaState {
                                        session_token: self.token.clone(),
                                    });
                                }
                            }
                        }
                        Response::Users(v) => self.admin.users = v,
                        Response::AccountUsage(v) => self.account_usage = v,
                        Response::BrokerConfig {
                            config: v,
                            adapter_plugins,
                            adapter_override,
                        } => {
                            self.admin.adapter_plugins = adapter_plugins;
                            self.admin.adapter_override = adapter_override;
                            self.admin.models.saved_adapter_url = Some(v.adapter_url.clone());
                            self.admin.models.saved_model = v.model.clone();
                            self.admin.broker_config = v;
                        }
                        Response::BrokerMonitor(v) => self.admin.broker_monitor = Some(v),
                        Response::BrokerLog(v) => self.admin.admin_log = v,
                        Response::ModelSearchResults(v) => {
                            self.admin.models.results = v;
                            self.admin.models.selected_files.clear();
                        }
                        Response::OllamaState(v) => self.admin.ollama_state = Some(v),
                        Response::AdminDatabase(v) => self.admin.admin_data = v,
                        Response::GenerationStarted { character_id, .. } => {
                            self.active_request = Some(frame.request_id);
                            self.typing_character = Some(character_id);
                            self.generation_status = None;
                            self.stream_text.clear();
                        }
                        Response::GenerationStatus { status, .. }
                            if self.active_request == Some(frame.request_id) =>
                        {
                            self.generation_status = Some(status);
                        }
                        Response::GenerationFinished {
                            message_id,
                            revision,
                            cancelled,
                        } => {
                            self.pending_tts_message_id = (!cancelled).then_some(message_id);
                            self.finish_generation(revision);
                        }
                        Response::Accepted { revision, .. } => {
                            self.revision = self.revision.max(revision);
                            if self.memory_state.status_message.as_deref() == Some("Saving memory…")
                            {
                                self.memory_state.status_message = Some("Memory saved.".into());
                            } else if self.memory_state.status_message.as_deref()
                                == Some("Removing memory…")
                            {
                                self.memory_state.status_message = Some("Memory removed.".into());
                            } else if self.memory_state.status_message.as_deref()
                                == Some("Learning from chat…")
                            {
                                self.memory_state.status_message =
                                    Some("New memory learned from the chat.".into());
                            }
                            if !self.token.is_empty() {
                                self.refresh();
                                if self.memory_dialog_open {
                                    self.request_filtered_memories();
                                }
                                if let Some(id) = self.selected_conversation.clone() {
                                    self.open_conversation(&id);
                                }
                            }
                        }
                        Response::SyncComplete { revision } => {
                            self.revision = self.revision.max(revision);
                            self.refresh();
                            if let Some(id) = self.selected_conversation.clone() {
                                self.open_conversation(&id);
                            }
                        }
                        _ => {}
                    }
                }
            }
            MessageType::StreamChunk => {
                if let Ok(c) = decode::<StreamChunk>(&frame.payload) {
                    self.stream_text.push_str(&c.text);
                }
            }
            MessageType::StreamEnd => {
                if let Ok(Response::GenerationFinished {
                    message_id,
                    revision,
                    cancelled,
                }) = decode::<Response>(&frame.payload)
                {
                    self.pending_tts_message_id = (!cancelled).then_some(message_id);
                    self.finish_generation(revision);
                } else {
                    self.active_request = None;
                    self.typing_character = None;
                    self.generation_status = None;
                }
            }
            MessageType::Delta => {
                if let Ok(d) = decode::<StateDelta>(&frame.payload) {
                    self.apply_delta(d);
                }
            }
            MessageType::Error => {
                self.world_state.import_pending = false;
                self.world_state.import_started = None;
                self.char_import_pending = false;
                self.char_import_started = None;
                self.memory_state.condensing = false;
                if self
                    .memory_state
                    .status_message
                    .as_deref()
                    .is_some_and(|notice| notice.ends_with('…'))
                {
                    self.memory_state.status_message = None;
                }
                if let Ok(e) = decode::<WireError>(&frame.payload) {
                    let message = match e.code {
                        ErrorCode::BackendUnavailable | ErrorCode::ModelMissing => {
                            "The broker could not complete that request.".into()
                        }
                        _ => e.message,
                    };
                    self.set_error(message);
                    self.active_request = None;
                    self.typing_character = None;
                    self.generation_status = None;
                }
            }
            _ => {}
        }
    }

    pub(crate) fn apply_delta(&mut self, d: StateDelta) {
        let payload = if matches!(d.operation, DeltaOperation::Delete) {
            None
        } else {
            decode::<DeltaPayload>(&d.changed_fields).ok()
        };
        if d.entity_type == "world" {
            self.worlds.retain(|world| world.id != d.entity_id);
            if let Some(DeltaPayload::World(world)) = payload.as_ref() {
                self.worlds.push(world.clone());
            }
        }
        if d.entity_type == "memory" {
            self.memories.retain(|memory| memory.id != d.entity_id);
            if let Some(DeltaPayload::Memory(mut memory)) = payload.as_ref().cloned() {
                memory.revision = d.revision;
                self.memories.push(memory);
                self.memories.sort_by(|left, right| {
                    right
                        .pinned
                        .cmp(&left.pinned)
                        .then_with(|| right.updated_at.cmp(&left.updated_at))
                });
            }
        }
        if d.entity_type == "message" {
            if matches!(d.operation, DeltaOperation::Delete) {
                self.messages.retain(|message| message.id != d.entity_id);
            } else if let Some(DeltaPayload::Message {
                conversation_id,
                author_type,
                author_id,
                content,
                parent_id,
                selected_variant_id,
                character_image_id,
            }) = payload.as_ref()
            {
                if self.selected_conversation.as_deref() == Some(conversation_id.as_str()) {
                    if let Some(message) = self
                        .messages
                        .iter_mut()
                        .find(|message| message.id == d.entity_id)
                    {
                        message.author_type.clone_from(author_type);
                        message.author_id.clone_from(author_id);
                        message.content.clone_from(content);
                        message.parent_id.clone_from(parent_id);
                        message.selected_variant_id.clone_from(selected_variant_id);
                        message.character_image_id.clone_from(character_image_id);
                        message.revision = d.revision;
                    } else if parent_id.is_none() {
                        self.messages.push(ChatMessage {
                            id: d.entity_id.clone(),
                            author_type: author_type.clone(),
                            author_id: author_id.clone(),
                            content: content.clone(),
                            parent_id: None,
                            selected_variant_id: selected_variant_id.clone(),
                            character_image_id: character_image_id.clone(),
                            created_at: String::new(),
                            revision: d.revision,
                            variants: Vec::new(),
                        });
                    }
                }
            }
        } else if d.entity_type == "variant" {
            if let Some(DeltaPayload::Variant {
                message_id,
                content,
                character_image_id,
            }) = payload.as_ref()
            {
                if let Some(message) = self
                    .messages
                    .iter_mut()
                    .find(|message| message.id == *message_id)
                {
                    if let Some(variant) = message
                        .variants
                        .iter_mut()
                        .find(|variant| variant.id == d.entity_id)
                    {
                        variant.content.clone_from(content);
                        variant.revision = d.revision;
                    } else {
                        message.variants.push(Variant {
                            id: d.entity_id.clone(),
                            content: content.clone(),
                            created_at: String::new(),
                            revision: d.revision,
                        });
                    }
                    message.selected_variant_id = Some(d.entity_id.clone());
                    message.character_image_id.clone_from(character_image_id);
                    message.revision = d.revision;
                }
            }
        }
        self.revision = self.revision.max(d.revision);
        let key = (d.entity_type, d.entity_id);
        if matches!(d.operation, DeltaOperation::Delete) {
            self.state.remove(&key);
        } else if let Some(p) = payload {
            self.state.insert(key, p);
        }
    }

    pub(crate) fn finish_generation(&mut self, revision: i64) {
        self.revision = self.revision.max(revision);
        self.active_request = None;
        self.typing_character = None;
        self.generation_status = None;
        self.send(Request::GetAccountUsage {
            session_token: self.token.clone(),
        });
        if let Some(id) = self.selected_conversation.clone() {
            self.open_conversation(&id);
        }
    }

    pub(crate) fn open_conversation(&mut self, id: &str) {
        self.auto_select_conversation = true;
        self.selected_conversation = Some(id.into());
        self.send(Request::GetConversation {
            session_token: self.token.clone(),
            conversation_id: id.into(),
        });
    }

    pub(crate) fn submit_message(&mut self) {
        let Some(cid) = self.selected_conversation.clone() else {
            self.new_chat_open = true;
            return;
        };
        let speaker = self.selected_speaker().map(str::to_owned);
        if self.input.trim().is_empty() {
            self.send(Request::Generate {
                session_token: self.token.clone(),
                conversation_id: cid,
                speaker_id: speaker,
                parent_id: None,
            });
            return;
        }
        let content = std::mem::take(&mut self.input);
        let message = Request::SendMessage {
            session_token: self.token.clone(),
            conversation_id: cid.clone(),
            content,
            speaker_id: None,
        };
        let generate = Request::Generate {
            session_token: self.token.clone(),
            conversation_id: cid,
            speaker_id: speaker,
            parent_id: None,
        };
        let _ = self.commands.send(Command::SendThenGenerate {
            message: Box::new(message),
            generate: Box::new(generate),
        });
    }

    pub(crate) fn regenerate(&self, message: &ChatMessage) {
        if let Some(cid) = self.selected_conversation.clone() {
            self.send(Request::Generate {
                session_token: self.token.clone(),
                conversation_id: cid,
                speaker_id: message.author_id.clone(),
                parent_id: Some(message.id.clone()),
            });
        }
    }

    fn selected_speaker(&self) -> Option<&str> {
        self.selected_conversation
            .as_deref()
            .and_then(|id| self.conversations.iter().find(|c| c.id == id))
            .and_then(|c| c.participant_ids.first().map(String::as_str))
    }

    pub(crate) fn displayed_message_content(message: &ChatMessage) -> &str {
        conversation::displayed_message_content(message)
    }

    #[allow(dead_code)]
    pub(crate) fn current_emotion_portrait(&self) -> Option<(String, String, String, Vec<u8>)> {
        conversation::current_emotion_portrait(
            &self.characters,
            &self.messages,
            self.selected_conversation.as_deref(),
            self.selected_speaker(),
        )
    }

    pub(crate) fn open_memory_manager(
        &mut self,
        character_id: Option<String>,
        conversation_id: Option<String>,
    ) {
        self.memory_state = MemoryDialogState::with_filters(character_id, conversation_id);
        self.memory_dialog_open = true;
        self.request_filtered_memories();
    }

    pub(crate) fn request_filtered_memories(&self) {
        self.send(Request::ListMemories {
            session_token: self.token.clone(),
            conversation_id: self.memory_state.filter_conversation_id.clone(),
            character_id: self.memory_state.filter_character_id.clone(),
        });
    }

    pub(crate) fn load_admin_tab(&mut self) {
        if self.admin.admin_tab == 2 && self.admin.models.busy {
            return;
        }
        for req in admin_monitor::requests_for_tab(self.admin.admin_tab, &self.token) {
            self.send(req);
        }
        if self.admin.admin_tab == 0 {
            self.last_monitor_refresh = Some(Instant::now());
        }
    }

    pub(crate) fn refresh_admin_monitor_if_due(&mut self) {
        if self.screen != Screen::Admin || self.admin.admin_tab != 0 || self.token.is_empty() {
            self.last_monitor_refresh = None;
            return;
        }
        let interval = Duration::from_secs(2);
        if self
            .last_monitor_refresh
            .is_none_or(|last| last.elapsed() >= interval)
        {
            self.send(Request::AdminGetBrokerMonitor {
                session_token: self.token.clone(),
            });
            self.last_monitor_refresh = Some(Instant::now());
        }
    }

    pub(crate) fn save_preferences(&self) {
        if let Some(path) = &self.preferences_path {
            save_preferences(path, self.tts_auto_speak);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn top_center_square_uv(width: f32, height: f32) -> (Point, Point) {
        if width > height {
            let margin = (1.0 - height / width) / 2.0;
            (Point::new(margin, 0.0), Point::new(1.0 - margin, 1.0))
        } else {
            (Point::new(0.0, 0.0), Point::new(1.0, width / height))
        }
    }

    pub(crate) fn load_inspection_demo(&mut self) {
        self.role = Some(Role::Admin);
        self.token = "demo-session".into();
        let pixels = image::RgbaImage::from_fn(128, 128, |x, y| {
            let dx = (x as f32) - 64.0;
            let dy = (y as f32) - 64.0;
            if dx * dx + dy * dy <= 54.0 * 54.0 {
                image::Rgba([79, 70, 229, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();

        self.characters = vec![Character {
            id: "assistant".into(),
            name: "Mara".into(),
            description: "A thoughtful research assistant stationed at an alpine observatory."
                .into(),
            personality: "Curious, warm, precise, and observant.".into(),
            scenario: "Midnight review in the high library.".into(),
            system_prompt: "Adopt a grounded, literate, direct tone.".into(),
            example_dialogue: String::new(),
            appearance: "Dark braided hair, knitted wool sweater, silver wire spectacles.".into(),
            age: "28".into(),
            gender: "female".into(),
            race: "human".into(),
            misc: String::new(),
            tags: vec!["astronomy".into(), "companion".into(), "ambient".into()],
            avatar: None,
            images: vec![CharacterImage {
                id: "demo-neutral".into(),
                label: "neutral".into(),
                data: encoded.into_inner(),
            }],
            default_image_id: Some("demo-neutral".into()),
            is_public: true,
            owned_by_user: true,
            revision: 1,
        }];
        self.conversations = vec![Conversation {
            id: "demo".into(),
            title: "Night watch at the observatory".into(),
            participant_ids: vec!["assistant".into()],
            kind: ConversationKind::Direct,
            state: String::new(),
            summary: String::new(),
            revision: 1,
        }];
        self.selected_conversation = Some("demo".into());
        self.messages.push(ChatMessage {
            id: "m1".into(),
            author_type: "user".into(),
            author_id: None,
            content: "Tell me about this place.".into(),
            parent_id: None,
            selected_variant_id: None,
            character_image_id: None,
            created_at: String::new(),
            revision: 1,
            variants: vec![],
        });
        self.memories = vec![
            MemoryEntry {
                id: "memory-pinned".into(),
                conversation_id: None,
                character_id: Some("assistant".into()),
                kind: MemoryKind::Fact,
                content: "The user feels calmer when plans are explained one step at a time."
                    .into(),
                importance: 90,
                pinned: true,
                confidence: 1.0,
                source: MemorySource::Manual,
                source_message_ids: vec![],
                created_at: "2026-08-20 09:10:00".into(),
                updated_at: "2026-08-20 09:10:00".into(),
                revision: 2,
            },
            MemoryEntry {
                id: "memory-event".into(),
                conversation_id: Some("demo".into()),
                character_id: Some("assistant".into()),
                kind: MemoryKind::Event,
                content: "Mara and the user discovered the old observatory above the valley."
                    .into(),
                importance: 70,
                pinned: false,
                confidence: 0.88,
                source: MemorySource::Automatic,
                source_message_ids: vec!["m1".into(), "m2".into()],
                created_at: "2026-08-24 14:30:00".into(),
                updated_at: "2026-08-24 14:30:00".into(),
                revision: 3,
            },
        ];
        self.messages.push(ChatMessage {
            id: "m2".into(),
            author_type: "character".into(),
            author_id: Some("assistant".into()),
            content:
                "# The old observatory\n\nIt has watched the valley for **two hundred years**."
                    .into(),
            parent_id: Some("m1".into()),
            selected_variant_id: None,
            character_image_id: Some("demo-neutral".into()),
            created_at: String::new(),
            revision: 1,
            variants: vec![],
        });
        self.admin.broker_monitor = Some(BrokerMonitor {
            uptime_seconds: 3725,
            cpu_percent: 1.8,
            memory_used_mb: 42,
            memory_limit_mb: Some(512),
            active_connections: 2,
            adapter_status: AdapterStatus::Online,
            adapter_model_count: 1,
            adapter_latency_ms: Some(18),
            recent_errors: vec!["2026-08-24 14:32:07 UTC · Adapter request timed out".into()],
        });
        self.admin.admin_log = BrokerLog {
            content: concat!(
                "2026-08-24T14:30:00Z  INFO chatty_broker: boot log initialized\n",
                "2026-08-24T14:30:01Z  INFO chatty_broker: TLS 1.3 broker listening address=0.0.0.0:7443\n",
                "2026-08-24T14:30:02Z  INFO chatty_broker: inference backend ready models=[\"llama3.2:3b\"]\n",
                "2026-08-24T14:32:07Z  WARN chatty_broker: connection closed peer=192.0.2.8 error=request timed out\n"
            )
            .into(),
            truncated: false,
            file_size_bytes: 412,
        };
        self.admin.adapter_plugins = vec![
            "freetoken".into(),
            "ollama".into(),
            "openai-compatible".into(),
        ];
        self.admin.broker_config.adapter_url = "http://192.0.2.50:11434".into();
        self.admin.models.saved_adapter_url = Some(self.admin.broker_config.adapter_url.clone());
        self.admin.models.downloads.insert(
            "demo-roleplay:27b".into(),
            admin_monitor::ModelDownload {
                destination: self.admin.broker_config.adapter_url.clone(),
                progress: chatty_protocol::admin::ModelDownloadProgress {
                    status: "pulling layer".into(),
                    digest: Some("sha256:demo-layer".into()),
                    total: Some(20_000_000_000),
                    completed: Some(7_500_000_000),
                },
                ..Default::default()
            },
        );
        self.admin.models.downloads.insert(
            "demo-story:9b".into(),
            admin_monitor::ModelDownload {
                destination: self.admin.broker_config.adapter_url.clone(),
                progress: chatty_protocol::admin::ModelDownloadProgress {
                    status: "pulling manifest".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        self.admin.models.results = vec![ModelSearchResult {
            id: "bartowski/Llama-3.2-1B-Instruct-GGUF".into(),
            downloads: 12345,
            files: vec![
                "Llama-3.2-1B-Instruct-Q4_K_M.gguf".into(),
                "Llama-3.2-1B-Instruct-Q8_0.gguf".into(),
            ],
        }];
        self.admin.ollama_state = Some(OllamaState {
            version: "0.12.6".into(),
            models: vec![
                OllamaModel {
                    name: "llama3.2:3b".into(),
                    size: 2_018_000_000,
                    modified_at: "2026-08-23T18:30:00Z".into(),
                    family: "llama".into(),
                    parameter_size: "3.2B".into(),
                    quantization_level: "Q4_K_M".into(),
                },
                OllamaModel {
                    name: "gemma3:4b".into(),
                    size: 3_330_000_000,
                    modified_at: "2026-08-22T12:00:00Z".into(),
                    family: "gemma3".into(),
                    parameter_size: "4.3B".into(),
                    quantization_level: "Q4_K_M".into(),
                },
            ],
            running_models: vec![OllamaRunningModel {
                name: "llama3.2:3b".into(),
                size: 2_018_000_000,
                size_vram: 1_840_000_000,
                expires_at: "2026-08-24T15:05:00Z".into(),
            }],
        });
    }
}

pub(crate) fn broker_host(value: &str) -> Option<String> {
    let value = value.trim();
    value
        .parse::<IpAddr>()
        .map(|ip| ip.to_string())
        .ok()
        .or_else(|| {
            value
                .parse::<SocketAddr>()
                .map(|address| address.ip().to_string())
                .ok()
        })
        .or_else(|| {
            let host = match value.rsplit_once(':') {
                Some((host, "7443")) if !host.contains(':') => host,
                Some(_) => return None,
                None => value,
            };
            let valid = !host.is_empty()
                && host.len() <= 253
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .as_bytes()
                            .first()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label
                            .as_bytes()
                            .last()
                            .is_some_and(u8::is_ascii_alphanumeric)
                        && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                });
            valid.then(|| host.to_ascii_lowercase())
        })
}

pub(crate) fn connection_target(value: &str) -> Option<ConnectionTarget> {
    let host = broker_host(value)?;
    let broker = host.parse::<IpAddr>().map_or_else(
        |_| format!("{host}:7443"),
        |ip| SocketAddr::new(ip, 7443).to_string(),
    );
    Some(ConnectionTarget {
        broker,
        server_name: host,
    })
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| format_err!("failed to install TLS provider"))?;

    let parsed = ParsedArgs::parse(USAGE)?;
    if parsed.flag("test-speech") {
        test_speech().map_err(|e| format_err!("{e}"))?;
        return Ok(());
    }

    let args = Args {
        broker: parsed.string("broker", "CHATTY_BROKER", ""),
        ca: parsed.optional("ca", "CHATTY_CA").map(PathBuf::from),
        inspect: parsed.flag("inspect"),
        inspect_control: parsed
            .optional("inspect-control", "CHATTY_INSPECT_CONTROL")
            .map(PathBuf::from),
        width: parsed.number("width", 1100.0),
        height: parsed.number("height", 720.0),
        session_file: parsed
            .optional("session-file", "CHATTY_SESSION_FILE")
            .map(PathBuf::from),
        test_speech: false,
    };

    let width = args.width;
    let height = args.height;
    let args_arc = Arc::new(args);
    let boot_args = args_arc.clone();

    iced::application(
        move || ChattyApp::boot((*boot_args).clone()),
        ChattyApp::update,
        ChattyApp::view,
    )
    .subscription(ChattyApp::subscription)
    .theme(ChattyApp::theme)
    .style(|_, theme| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    })
    .scale_factor(ChattyApp::scale_factor)
    .window(window::Settings {
        size: Size::new(width, height),
        min_size: None,
        transparent: true,
        ..Default::default()
    })
    .run()
    .map_err(|e| Error::msg(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_host_accepts_ips_and_domains() {
        assert_eq!(broker_host("192.168.0.98"), Some("192.168.0.98".into()));
        assert_eq!(
            broker_host("chatty.example.com"),
            Some("chatty.example.com".into())
        );
        assert_eq!(
            broker_host("192.168.0.98:7443"),
            Some("192.168.0.98".into())
        );
        assert_eq!(
            broker_host("chatty.example.com:7443"),
            Some("chatty.example.com".into())
        );
        assert_eq!(broker_host("https://chatty.example.com"), None);
        assert_eq!(broker_host("chatty.example.com:8080"), None);
        assert_eq!(broker_host("-bad.domain"), None);
    }

    #[test]
    fn connection_target_uses_tls_host_and_default_port() {
        let ipv4 = connection_target("192.168.0.98").unwrap();
        assert_eq!(ipv4.broker, "192.168.0.98:7443");
        assert_eq!(ipv4.server_name, "192.168.0.98");

        let domain = connection_target("chatty.example.com").unwrap();
        assert_eq!(domain.broker, "chatty.example.com:7443");
        assert_eq!(domain.server_name, "chatty.example.com");
    }

    #[test]
    fn portrait_crop_is_top_aligned_and_horizontally_centered() {
        let (min, max) = ChattyApp::top_center_square_uv(256.0, 512.0);
        assert_eq!(min, Point::new(0.0, 0.0));
        assert_eq!(max, Point::new(1.0, 0.5));

        let (min, max) = ChattyApp::top_center_square_uv(512.0, 256.0);
        assert_eq!(min, Point::new(0.25, 0.0));
        assert_eq!(max, Point::new(0.75, 1.0));
    }

    #[test]
    fn continue_creates_a_message_and_retry_creates_a_variant() {
        let (commands, mut command_rx) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        app.load_inspection_demo();

        app.submit_message();
        assert!(matches!(
            command_rx.try_recv().unwrap(),
            Command::Request(request)
                if matches!(&*request, Request::Generate { parent_id: None, .. })
        ));

        let response = app.messages.last().unwrap().clone();
        app.regenerate(&response);
        assert!(matches!(
            command_rx.try_recv().unwrap(),
            Command::Request(request)
                if matches!(&*request, Request::Generate { parent_id: Some(parent), .. }
                    if parent == &response.id)
        ));
    }

    #[test]
    fn selected_variant_content_is_displayed_after_reload() {
        let mut message = ChatMessage {
            id: "message".into(),
            author_type: "character".into(),
            author_id: Some("assistant".into()),
            content: "original".into(),
            parent_id: None,
            selected_variant_id: Some("retry".into()),
            character_image_id: Some("happy".into()),
            created_at: String::new(),
            revision: 2,
            variants: vec![Variant {
                id: "retry".into(),
                content: "replacement".into(),
                created_at: String::new(),
                revision: 2,
            }],
        };
        assert_eq!(
            ChattyApp::displayed_message_content(&message),
            "replacement"
        );

        message.selected_variant_id = Some("missing".into());
        assert_eq!(ChattyApp::displayed_message_content(&message), "original");
    }

    #[test]
    fn latest_persisted_reply_changes_the_visible_emotion() {
        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        app.load_inspection_demo();

        let pixels = image::RgbaImage::from_pixel(128, 128, image::Rgba([40, 180, 90, 255]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        app.characters[0].images.push(CharacterImage {
            id: "demo-happy".into(),
            label: "happy".into(),
            data: encoded.into_inner(),
        });
        app.messages.push(ChatMessage {
            id: "m3".into(),
            author_type: "character".into(),
            author_id: Some("assistant".into()),
            content: "A newly persisted reply".into(),
            parent_id: None,
            selected_variant_id: None,
            character_image_id: Some("demo-happy".into()),
            created_at: String::new(),
            revision: 3,
            variants: vec![],
        });

        let portrait = app.current_emotion_portrait().unwrap();
        assert_eq!(portrait.1, "happy");
        assert_eq!(portrait.2, "demo-happy");
    }

    #[test]
    fn generation_delta_updates_the_emotion_panel_immediately() {
        fn emotion_delta() -> Frame {
            let changed_fields = encode(&DeltaPayload::Message {
                conversation_id: "demo".into(),
                author_type: "character".into(),
                author_id: Some("assistant".into()),
                content: "The generated reply remains visible".into(),
                parent_id: None,
                selected_variant_id: None,
                character_image_id: Some("demo-happy".into()),
            })
            .unwrap();
            Frame {
                compressed: false,
                message_type: MessageType::Delta,
                request_id: 42,
                payload: encode(&StateDelta {
                    revision: 4,
                    entity_type: "message".into(),
                    entity_id: "generated-reply".into(),
                    operation: DeltaOperation::Add,
                    changed_fields,
                })
                .unwrap(),
            }
        }

        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        app.load_inspection_demo();

        let pixels = image::RgbaImage::from_pixel(128, 128, image::Rgba([40, 180, 90, 255]));
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        app.characters[0].images.push(CharacterImage {
            id: "demo-happy".into(),
            label: "happy".into(),
            data: encoded.into_inner(),
        });

        app.handle_frame(emotion_delta());

        assert_eq!(
            app.messages.last().map(|m| m.content.as_str()),
            Some("The generated reply remains visible")
        );
        let portrait = app.current_emotion_portrait().unwrap();
        assert_eq!(portrait.1, "happy");
        assert_eq!(portrait.2, "demo-happy");
    }

    #[test]
    fn admin_monitor_refreshes_only_while_open() {
        let (commands, mut command_rx) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        app.token = "session".into();
        app.screen = Screen::Admin;
        app.admin.admin_tab = 0;
        app.last_monitor_refresh = Some(Instant::now() - Duration::from_secs(3));
        app.refresh_admin_monitor_if_due();
        assert!(matches!(
            command_rx.try_recv(),
            Ok(Command::Request(request))
                if matches!(*request, Request::AdminGetBrokerMonitor { .. })
        ));

        app.screen = Screen::Chat;
        app.last_monitor_refresh = Some(Instant::now() - Duration::from_secs(3));
        app.refresh_admin_monitor_if_due();
        assert!(command_rx.try_recv().is_err());
    }

    #[test]
    fn long_network_errors_move_to_the_status_tooltip() {
        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        let detail = "2026-09-14 15:43:39 UTC · Offline: connection timed out";
        app.set_network_status(detail.into());
        assert_eq!(app.status, "Offline");
        assert_eq!(app.status_tooltip.as_deref(), Some(detail));
        app.set_network_status("Online · TLS 1.3".into());
        assert_eq!(app.status, "Online · TLS 1.3");
        assert!(app.status_tooltip.is_none());
    }

    #[test]
    fn view_builds_without_panics_for_all_states() {
        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);

        // Login screen
        let _ = app.view_at_size(Size::new(360.0, 300.0));

        // Connected demo chat screen
        app.load_inspection_demo();
        app.connected = true;
        app.token = "test-token".into();
        app.screen = Screen::Chat;
        let _ = app.view_at_size(Size::new(360.0, 300.0));

        // Modals
        app.new_chat_open = true;
        let _ = app.view_at_size(Size::new(360.0, 300.0));
        app.new_chat_open = false;

        app.draft_character_open = true;
        let _ = app.view_at_size(Size::new(360.0, 300.0));
        app.draft_character_open = false;

        app.worlds_open = true;
        let _ = app.view_at_size(Size::new(360.0, 300.0));
        app.worlds_open = false;

        app.memory_dialog_open = true;
        let _ = app.view_at_size(Size::new(360.0, 300.0));
        app.memory_dialog_open = false;

        app.screen = Screen::Admin;
        let _ = app.view_at_size(Size::new(360.0, 300.0));

        app.screen = Screen::Settings;
        let _ = app.view_at_size(Size::new(360.0, 300.0));
    }
    #[test]
    fn model_request_errors_recover_only_the_matching_operation() {
        let (commands, _) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        app.admin.models.busy = true;
        app.admin.models.request_id = Some(42);
        let payload = encode(&WireError {
            code: ErrorCode::BackendUnavailable,
            message: "Download failed: disk full".into(),
            retryable: true,
        })
        .unwrap();
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Error,
            request_id: 41,
            payload: payload.clone(),
        });
        assert!(app.admin.models.busy);
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Error,
            request_id: 42,
            payload,
        });
        assert!(!app.admin.models.busy);
        assert_eq!(
            app.admin.models.notice.as_deref(),
            Some("Download failed: disk full")
        );
        app.admin.models.busy = true;
        app.handle_event(Event::Status("Offline: connection lost".into()));
        assert!(!app.admin.models.busy);
    }

    #[test]
    fn download_progress_cancellation_and_errors_are_request_scoped() {
        let (commands, mut sent) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        for model in ["a", "b"] {
            app.admin
                .update(AdminMessage::DownloadModel(model.into()), "session");
        }
        app.handle_event(Event::DownloadStarted {
            request_id: 41,
            model: "a".into(),
        });
        app.handle_event(Event::DownloadStarted {
            request_id: 42,
            model: "b".into(),
        });
        let progress = chatty_protocol::admin::ModelDownloadProgress {
            status: "pulling layer".into(),
            total: Some(100),
            completed: Some(25),
            digest: None,
        };
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 41,
            payload: encode(&Response::ModelDownloadProgress(progress)).unwrap(),
        });
        assert_eq!(app.admin.models.downloads["a"].progress.completed, Some(25));
        assert_eq!(app.admin.models.downloads["b"].progress.completed, None);
        assert!(!app.admin.models.busy);
        let _ = app.update(Message::Admin(AdminMessage::CancelDownload("a".into())));
        assert!(matches!(sent.try_recv().unwrap(), Command::Cancel(41)));
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 41,
            payload: encode(&Response::ModelDownloadFinished { cancelled: true }).unwrap(),
        });
        assert!(app.admin.models.downloads["a"].finished);
        assert!(!app.admin.models.downloads["b"].finished);
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Error,
            request_id: 42,
            payload: encode(&WireError {
                code: ErrorCode::BackendUnavailable,
                message: "disk full".into(),
                retryable: false,
            })
            .unwrap(),
        });
        assert_eq!(app.admin.models.downloads["b"].progress.status, "disk full");
        assert!(app.admin.models.downloads["b"].finished);

        // Cancelling before the network assigns an ID must still cancel upstream.
        app.admin
            .update(AdminMessage::DownloadModel("a".into()), "session");
        let _ = app.update(Message::Admin(AdminMessage::CancelDownload("a".into())));
        assert!(sent.try_recv().is_err());
        app.handle_event(Event::DownloadStarted {
            request_id: 43,
            model: "a".into(),
        });
        assert!(matches!(sent.try_recv().unwrap(), Command::Cancel(43)));
        // A late terminal event from the previous attempt cannot finish its retry.
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 41,
            payload: encode(&Response::ModelDownloadFinished { cancelled: false }).unwrap(),
        });
        assert!(!app.admin.models.downloads["a"].finished);
        app.handle_event(Event::Status("Offline: connection lost".into()));
        assert!(app.admin.models.downloads["a"].finished);
    }

    #[test]
    fn memory_condense_flow_updates_status_and_recovers_on_error() {
        let (commands, mut sent) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        let mut app = ChattyApp::new(commands, events);
        app.token = "session-token".into();
        app.memory_state.filter_character_id = Some("char-1".into());
        app.memory_state.filter_conversation_id = Some("conv-1".into());
        app.memory_state.filter_kind = Some(chatty_protocol::MemoryKind::Fact);

        let _ = app.update(Message::Memory(MemoryMessage::Condense));
        assert!(app.memory_state.condensing);
        assert_eq!(
            app.memory_state.status_message.as_deref(),
            Some("Condensing memories…")
        );
        match sent.try_recv().unwrap() {
            Command::Request(req) => match *req {
                Request::CondenseMemories {
                    session_token,
                    conversation_id,
                    character_id,
                    kind,
                } => {
                    assert_eq!(session_token, "session-token");
                    assert_eq!(conversation_id.as_deref(), Some("conv-1"));
                    assert_eq!(character_id.as_deref(), Some("char-1"));
                    assert_eq!(kind, Some(chatty_protocol::MemoryKind::Fact));
                }
                other => panic!("unexpected request: {other:?}"),
            },
            other => panic!("unexpected command: {other:?}"),
        }

        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 1,
            payload: encode(&Response::MemoriesCondensed {
                before: 4,
                after: 2,
            })
            .unwrap(),
        });
        assert!(!app.memory_state.condensing);
        assert_eq!(
            app.memory_state.status_message.as_deref(),
            Some("Condensed 4 memories into 2.")
        );
        match sent.try_recv().unwrap() {
            Command::Request(req) => assert!(matches!(*req, Request::GetAccountUsage { .. })),
            other => panic!("unexpected command: {other:?}"),
        }

        let _ = app.update(Message::Memory(MemoryMessage::Condense));
        assert!(app.memory_state.condensing);
        let _ = sent.try_recv().unwrap();
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Error,
            request_id: 2,
            payload: encode(&WireError {
                code: ErrorCode::BackendUnavailable,
                message: "model busy".into(),
                retryable: true,
            })
            .unwrap(),
        });
        assert!(!app.memory_state.condensing);
        assert_eq!(app.memory_state.status_message, None);
    }
}
