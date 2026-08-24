#![allow(clippy::collapsible_if)]

use anyhow::{Context, Result};
use chatty_protocol::*;
use clap::Parser;
use eframe::egui;
use rustls::{ClientConfig, RootCertStore, pki_types::ServerName};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::BufReader,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use tokio::{net::TcpStream, sync::mpsc};
use tokio_rustls::{TlsConnector, client::TlsStream};

mod admin_monitor;
mod characters;
mod conversation;
mod network;
mod ui;
use network::{Command, ConnectionTarget, Event};
use ui::FooterIcon;

const COLOR_PRIMARY: egui::Color32 = egui::Color32::from_rgb(99, 102, 241);
const COLOR_PRIMARY_STRONG: egui::Color32 = egui::Color32::from_rgb(79, 70, 229);
const COLOR_ON_PRIMARY: egui::Color32 = egui::Color32::WHITE;

fn color_surface_raised(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().faint_bg_color
}

fn color_border(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().widgets.noninteractive.bg_stroke.color
}

fn color_primary_text(ui: &egui::Ui) -> egui::Color32 {
    if ui.visuals().dark_mode {
        egui::Color32::from_rgb(199, 210, 254)
    } else {
        egui::Color32::from_rgb(49, 46, 129)
    }
}

fn paint_glass_background(ui: &egui::Ui, light_mode: bool) {
    ui.painter().rect_filled(
        ui.max_rect(),
        0.0,
        if light_mode {
            egui::Color32::from_rgba_unmultiplied(226, 232, 248, 175)
        } else {
            egui::Color32::from_rgba_unmultiplied(4, 8, 18, 205)
        },
    );
}

fn paint_glass_modal_scrim(ui: &egui::Ui, light_mode: bool) {
    ui.painter().rect_filled(
        ui.max_rect(),
        0.0,
        if light_mode {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 205)
        } else {
            egui::Color32::from_rgba_unmultiplied(0, 0, 0, 210)
        },
    );
}

fn modal_frame(ctx: &egui::Context, light_mode: bool, glass_mode: bool) -> egui::Frame {
    let theme = if light_mode {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    };
    let frame = egui::Frame::window(&ctx.style_of(theme));
    if glass_mode {
        frame.fill(if light_mode {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180)
        } else {
            egui::Color32::from_rgba_unmultiplied(20, 29, 48, 180)
        })
    } else {
        frame
    }
}

#[derive(Parser, Clone)]
struct Args {
    #[arg(long, env = "CHATTY_BROKER", default_value = "")]
    broker: String,
    #[arg(long, env = "CHATTY_CA")]
    ca: Option<PathBuf>,
    #[arg(long)]
    inspect: bool,
    #[arg(long, default_value = "/tmp/chatty-gui-control")]
    inspect_control: PathBuf,
    #[arg(long, default_value_t = 1100.0)]
    width: f32,
    #[arg(long, default_value_t = 720.0)]
    height: f32,
    #[arg(long, env = "CHATTY_SESSION_FILE")]
    session_file: Option<PathBuf>,
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("failed to install TLS provider"))?;
    let mut args = Args::parse();
    let inspect = args.inspect;
    let mut initial_server = broker_host(&args.broker).unwrap_or_default();
    let mut initial_broker = connection_target(&initial_server).map(|target| target.broker);
    let mut remembered_session = None;
    if !inspect {
        // Resolve this before starting eframe so debug and release launches use
        // the same absolute XDG state path and never fall back to the cwd.
        args.session_file = Some(network::session_path(&args)?);
        if initial_server.is_empty() {
            initial_server = args
                .session_file
                .as_deref()
                .map(network::last_server_path)
                .and_then(|path| network::load_last_server(&path))
                .and_then(|server| broker_host(&server))
                .unwrap_or_default();
            initial_broker = connection_target(&initial_server).map(|target| target.broker);
        }
        remembered_session = args
            .session_file
            .as_ref()
            .and_then(|path| network::load_session(path, initial_broker.as_deref()));
    }
    let preferences_path = args.session_file.as_deref().map(network::preferences_path);
    let light_mode = preferences_path
        .as_deref()
        .is_some_and(network::load_light_mode);
    let glass_mode = preferences_path
        .as_deref()
        .is_some_and(network::load_glass_mode);
    let transparency = preferences_path
        .as_deref()
        .map_or(20, network::load_transparency);
    let size = [args.width, args.height];
    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    if !inspect {
        thread::Builder::new()
            .name("chatty-network".into())
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(network::run(args, remembered_session, command_rx, event_tx))
            })?;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([640.0, 480.0])
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native(
        "Chatty",
        options,
        Box::new(move |cc| {
            configure_style_with_surface(&cc.egui_ctx, light_mode, glass_mode, transparency);
            let mut app = ChattyApp::new(command_tx, event_rx);
            app.server_address = initial_server;
            app.light_mode = light_mode;
            app.glass_mode = glass_mode;
            app.transparency = transparency;
            app.preferences_path = preferences_path;
            if inspect {
                app.load_inspection_demo();
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}

fn broker_host(value: &str) -> Option<String> {
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

fn connection_target(value: &str) -> Option<ConnectionTarget> {
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

fn configure_style_with_surface(
    ctx: &egui::Context,
    light_mode: bool,
    glass_mode: bool,
    transparency: u8,
) {
    let transparency = transparency.min(80);
    let surface_alpha = 255_u8.saturating_sub(((u16::from(transparency) * 255) / 100) as u8);
    let raised_alpha = surface_alpha.saturating_add(18);
    let hover_alpha = surface_alpha.saturating_add(36);
    let mut visuals = if light_mode {
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(246, 248, 255, surface_alpha)
        } else {
            egui::Color32::from_rgb(246, 247, 251)
        };
        visuals.window_fill = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, raised_alpha)
        } else {
            egui::Color32::WHITE
        };
        visuals.extreme_bg_color = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(230, 235, 247, surface_alpha)
        } else {
            egui::Color32::from_rgb(238, 240, 246)
        };
        visuals.faint_bg_color = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, surface_alpha)
        } else {
            egui::Color32::from_rgb(235, 237, 245)
        };
        visuals.code_bg_color = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(238, 242, 252, raised_alpha)
        } else {
            egui::Color32::from_rgb(236, 238, 244)
        };
        visuals
    } else {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(10, 16, 29, surface_alpha)
        } else {
            egui::Color32::from_rgb(11, 15, 23)
        };
        visuals.window_fill = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(20, 29, 48, raised_alpha)
        } else {
            egui::Color32::from_rgb(17, 23, 34)
        };
        visuals.extreme_bg_color = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(7, 12, 23, surface_alpha)
        } else {
            egui::Color32::from_rgb(8, 12, 19)
        };
        visuals.faint_bg_color = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(65, 82, 122, surface_alpha)
        } else {
            egui::Color32::from_rgb(24, 32, 46)
        };
        visuals.code_bg_color = if glass_mode {
            egui::Color32::from_rgba_unmultiplied(28, 40, 65, raised_alpha)
        } else {
            egui::Color32::from_rgb(20, 28, 41)
        };
        visuals
    };
    if glass_mode {
        let subtle_fill = if light_mode {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, surface_alpha)
        } else {
            egui::Color32::from_rgba_unmultiplied(52, 66, 98, surface_alpha)
        };
        let border = if light_mode {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 210)
        } else {
            egui::Color32::from_rgba_unmultiplied(176, 196, 235, 105)
        };
        visuals.widgets.noninteractive.bg_fill = subtle_fill;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border);
        visuals.widgets.inactive.bg_fill = subtle_fill;
        visuals.widgets.inactive.weak_bg_fill = subtle_fill;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, border);
        visuals.widgets.hovered.bg_fill = if light_mode {
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, hover_alpha)
        } else {
            egui::Color32::from_rgba_unmultiplied(83, 103, 150, hover_alpha)
        };
        visuals.widgets.hovered.weak_bg_fill = visuals.widgets.hovered.bg_fill;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, border);
    }
    visuals.selection.bg_fill = egui::Color32::from_rgb(37, 99, 235);
    visuals.selection.stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(191, 219, 254));
    visuals.hyperlink_color = if light_mode {
        egui::Color32::from_rgb(29, 78, 216)
    } else {
        egui::Color32::from_rgb(96, 165, 250)
    };
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(37, 99, 235);
    visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(30, 64, 175);
    visuals.widgets.active.bg_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgb(96, 165, 250));
    visuals.window_stroke = if glass_mode && light_mode {
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 220),
        )
    } else if glass_mode {
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(176, 196, 235, 115),
        )
    } else if light_mode {
        egui::Stroke::new(1.0, egui::Color32::from_rgb(203, 208, 220))
    } else {
        egui::Stroke::new(1.0, egui::Color32::from_rgb(54, 66, 86))
    };
    visuals.window_corner_radius = egui::CornerRadius::same(14);
    visuals.weak_text_alpha = 0.74;
    ctx.set_visuals(visuals);
    let theme = if light_mode {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    };
    let mut style = (*ctx.style_of(theme)).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 9.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.interact_size.y = 36.0;
    // Egui uses this margin on both sides of popup title text. Eight pixels
    // trims the title bar from roughly 60 px to 44 px (about one quarter).
    style.spacing.window_margin = egui::Margin::same(8);
    style.visuals.menu_corner_radius = egui::CornerRadius::same(10);
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::new(24.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::new(15.0, egui::FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::new(14.0, egui::FontFamily::Proportional),
    );
    ctx.set_style_of(theme, style);
}

#[derive(Default, Clone)]
struct DraftCharacter {
    id: Option<String>,
    name: String,
    personality: String,
    scenario: String,
    system_prompt: String,
    example_dialogue: String,
    appearance: String,
    tags: String,
    avatar: Option<Vec<u8>>,
    is_public: bool,
    owned_by_user: bool,
}

#[derive(Clone)]
struct UiNotice {
    timestamp: String,
    message: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Chat,
    Characters,
    Admin,
    Settings,
}

struct ChattyApp {
    commands: mpsc::UnboundedSender<Command>,
    events: std::sync::mpsc::Receiver<Event>,
    status: String,
    server_address: String,
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
    conversations: Vec<Conversation>,
    messages: Vec<ChatMessage>,
    selected_conversation: Option<String>,
    auto_select_conversation: bool,
    selected_characters: HashSet<String>,
    input: String,
    stream_text: String,
    typing_character: Option<String>,
    active_request: Option<u64>,
    screen: Screen,
    sidebar_visible: bool,
    error: Option<UiNotice>,
    draft: DraftCharacter,
    draft_character_open: bool,
    new_chat_open: bool,
    new_conversation_title: String,
    users: Vec<UserAccount>,
    broker_config: BrokerConfig,
    broker_monitor: Option<BrokerMonitor>,
    ollama_state: Option<OllamaState>,
    ollama_pull_model: String,
    admin_data: Vec<AdminDataRow>,
    admin_tab: usize,
    admin_new_username: String,
    admin_new_password: String,
    admin_new_role: Role,
    last_monitor_refresh: Option<Instant>,
    account_usage: TokenUsage,
    light_mode: bool,
    glass_mode: bool,
    transparency: u8,
    preferences_path: Option<PathBuf>,
}

impl ChattyApp {
    fn new(
        commands: mpsc::UnboundedSender<Command>,
        events: std::sync::mpsc::Receiver<Event>,
    ) -> Self {
        Self {
            commands,
            events,
            status: "Enter the server address to begin.".into(),
            server_address: String::new(),
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
            characters: vec![],
            conversations: vec![],
            messages: vec![],
            selected_conversation: None,
            auto_select_conversation: true,
            selected_characters: HashSet::new(),
            input: String::new(),
            stream_text: String::new(),
            typing_character: None,
            active_request: None,
            screen: Screen::Chat,
            sidebar_visible: true,
            error: None,
            draft: DraftCharacter {
                owned_by_user: true,
                ..Default::default()
            },
            draft_character_open: false,
            new_chat_open: false,
            new_conversation_title: String::new(),
            users: vec![],
            broker_config: BrokerConfig {
                adapter_enabled: false,
                adapter_url: String::new(),
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
            },
            broker_monitor: None,
            ollama_state: None,
            ollama_pull_model: String::new(),
            admin_data: vec![],
            admin_tab: 0,
            admin_new_username: String::new(),
            admin_new_password: String::new(),
            admin_new_role: Role::User,
            last_monitor_refresh: None,
            account_usage: TokenUsage::default(),
            light_mode: false,
            glass_mode: false,
            transparency: 20,
            preferences_path: None,
        }
    }
    fn send(&self, request: Request) {
        let _ = self.commands.send(Command::Request(Box::new(request)));
    }
    fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(UiNotice {
            timestamp: current_utc_timestamp(),
            message: message.into(),
        });
    }
    fn refresh(&self) {
        self.send(Request::ListCharacters {
            session_token: self.token.clone(),
        });
        self.send(Request::ListConversations {
            session_token: self.token.clone(),
        });
    }
    fn authenticated(&mut self, token: String, user_id: String, role: Role, revision: i64) {
        self.connected = true;
        self.connecting = false;
        self.restoring_session = false;
        self.token = token;
        self.user_id = user_id;
        self.role = Some(role);
        self.revision = revision;
        self.auto_select_conversation = true;
        self.status = "Online · TLS 1.3".into();
        self.password.clear();
        self.refresh();
        self.send(Request::GetAccountUsage {
            session_token: self.token.clone(),
        });
    }
    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Status(s) => self.status = s,
                Event::Connected { resuming_session } => {
                    self.connected = true;
                    self.connecting = false;
                    self.restoring_session = resuming_session;
                }
                Event::ConnectionFailed(message) => {
                    self.connected = false;
                    self.connecting = false;
                    self.restoring_session = false;
                    self.status = "Not connected".into();
                    self.set_error(message);
                }
                Event::Disconnected => {
                    self.connected = false;
                    self.connecting = false;
                    self.restoring_session = false;
                    self.token.clear();
                    self.role = None;
                    self.status = "Enter the server IP to begin.".into();
                }
                Event::SessionExpired => {
                    self.restoring_session = false;
                    self.token.clear();
                    self.role = None;
                    self.set_error("Saved session expired. Sign in again.");
                }
                Event::Frame(frame) => self.handle_frame(frame),
            }
            ctx.request_repaint();
        }
    }
    fn handle_frame(&mut self, frame: Frame) {
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
                        Response::Characters(v) => self.characters = v,
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
                                self.auto_select_conversation = false;
                                self.refresh();
                            }
                        }
                        Response::Users(v) => self.users = v,
                        Response::AccountUsage(v) => self.account_usage = v,
                        Response::BrokerConfig(v) => self.broker_config = v,
                        Response::BrokerMonitor(v) => self.broker_monitor = Some(v),
                        Response::OllamaState(v) => self.ollama_state = Some(v),
                        Response::AdminDatabase(v) => self.admin_data = v,
                        Response::GenerationStarted { character_id, .. } => {
                            self.active_request = Some(frame.request_id);
                            self.typing_character = Some(character_id);
                            self.stream_text.clear();
                        }
                        Response::GenerationFinished { revision, .. } => {
                            self.revision = self.revision.max(revision);
                            self.active_request = None;
                            self.typing_character = None;
                            self.send(Request::GetAccountUsage {
                                session_token: self.token.clone(),
                            });
                            if let Some(id) = self.selected_conversation.clone() {
                                self.open_conversation(&id)
                            }
                        }
                        Response::Accepted { revision, .. } => {
                            self.revision = self.revision.max(revision);
                            if !self.token.is_empty() {
                                self.refresh();
                                if let Some(id) = self.selected_conversation.clone() {
                                    self.open_conversation(&id)
                                }
                            }
                        }
                        Response::SyncComplete { revision } => {
                            self.revision = self.revision.max(revision);
                            self.refresh();
                            if let Some(id) = self.selected_conversation.clone() {
                                self.open_conversation(&id)
                            }
                        }
                        _ => {}
                    }
                }
            }
            MessageType::StreamChunk => {
                if let Ok(c) = decode::<StreamChunk>(&frame.payload) {
                    self.stream_text.push_str(&c.text)
                }
            }
            MessageType::StreamEnd => {
                self.active_request = None;
                self.typing_character = None;
            }
            MessageType::Delta => {
                if let Ok(d) = decode::<StateDelta>(&frame.payload) {
                    self.apply_delta(d)
                }
            }
            MessageType::Error => {
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
                }
            }
            _ => {}
        }
    }
    fn apply_delta(&mut self, d: StateDelta) {
        self.revision = self.revision.max(d.revision);
        let key = (d.entity_type, d.entity_id);
        if matches!(d.operation, DeltaOperation::Delete) {
            self.state.remove(&key);
        } else if let Ok(p) = decode::<DeltaPayload>(&d.changed_fields) {
            self.state.insert(key, p);
        }
    }
    fn open_conversation(&mut self, id: &str) {
        self.auto_select_conversation = true;
        self.selected_conversation = Some(id.into());
        self.send(Request::GetConversation {
            session_token: self.token.clone(),
            conversation_id: id.into(),
        });
    }
    fn load_inspection_demo(&mut self) {
        self.connected = true;
        self.connecting = false;
        self.restoring_session = false;
        self.status = "Online · inspection".into();
        self.token = "inspection".into();
        self.user_id = "admin".into();
        self.username = "admin".into();
        self.role = Some(Role::Admin);
        self.account_usage = TokenUsage {
            prompt_tokens: 128_450,
            completion_tokens: 34_921,
        };
        self.users = vec![
            UserAccount {
                id: "admin".into(),
                username: "admin".into(),
                role: Role::Admin,
                created_at: "2026-08-20 09:00:00 UTC".into(),
                usage: self.account_usage,
            },
            UserAccount {
                id: "reader".into(),
                username: "reader".into(),
                role: Role::User,
                created_at: "2026-08-22 11:30:00 UTC".into(),
                usage: TokenUsage {
                    prompt_tokens: 8_412,
                    completion_tokens: 2_301,
                },
            },
        ];
        self.characters.push(Character {
            id: "assistant".into(),
            name: "Mara".into(),
            personality: "Warm and observant".into(),
            scenario: String::new(),
            system_prompt: String::new(),
            example_dialogue: String::new(),
            appearance: "Silver-haired traveller".into(),
            tags: vec!["demo".into()],
            avatar: None,
            is_public: true,
            owned_by_user: true,
            revision: 1,
        });
        self.conversations.push(Conversation {
            id: "demo".into(),
            title: "A quiet evening".into(),
            kind: ConversationKind::Direct,
            participant_ids: vec!["assistant".into()],
            state: String::new(),
            summary: String::new(),
            revision: 1,
        });
        self.selected_conversation = Some("demo".into());
        self.messages.push(ChatMessage {
            id: "m1".into(),
            author_type: "user".into(),
            author_id: None,
            content: "Tell me about this place.".into(),
            parent_id: None,
            selected_variant_id: None,
            created_at: String::new(),
            revision: 1,
            variants: vec![],
        });
        self.messages.push(ChatMessage {
            id: "m2".into(),
            author_type: "character".into(),
            author_id: Some("assistant".into()),
            content:
                "# The old observatory\n\nIt has watched the valley for **two hundred years**."
                    .into(),
            parent_id: Some("m1".into()),
            selected_variant_id: None,
            created_at: String::new(),
            revision: 1,
            variants: vec![],
        });
        self.broker_monitor = Some(BrokerMonitor {
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
        self.ollama_state = Some(OllamaState {
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

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod visual_tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[test]
    fn broker_host_accepts_ips_and_domains() {
        assert_eq!(broker_host("192.168.0.98"), Some("192.168.0.98".into()));
        assert_eq!(
            broker_host("192.168.0.98:7443"),
            Some("192.168.0.98".into())
        );
        assert_eq!(broker_host("Chatty.Example"), Some("chatty.example".into()));
        assert_eq!(broker_host("bad_name.example"), None);
        assert_eq!(broker_host("chatty.example:1234"), None);
    }

    #[test]
    fn connection_target_uses_tls_host_and_default_port() {
        assert_eq!(
            connection_target("chatty.example"),
            Some(ConnectionTarget {
                broker: "chatty.example:7443".into(),
                server_name: "chatty.example".into(),
            })
        );
    }

    fn harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ChattyApp> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app
            })
    }

    const LONG_USER_MESSAGE: &str = "Please write a detailed description of the observatory, including the weathered stone walls, the old brass telescope, the valley below, and every small detail that makes this place feel lived in and memorable.";

    fn long_user_message_harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ChattyApp> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                if size.x < 760.0 {
                    app.sidebar_visible = false;
                }
                app.messages[0].content = LONG_USER_MESSAGE.into();
                app
            })
    }

    #[test]
    fn visual_desktop_long_user_message() {
        let mut harness = long_user_message_harness(egui::vec2(1440.0, 900.0));
        harness.run_ok();
        let message = harness.get_by_label(LONG_USER_MESSAGE).rect();
        assert!(message.left() >= 0.0 && message.right() <= 1440.0);
        assert!(message.height() > 30.0);
        harness
            .render()
            .expect("render desktop long user message")
            .save("/tmp/chatty-desktop-long-user-message.png")
            .expect("save desktop long user message");
    }

    #[test]
    fn visual_compact_long_user_message() {
        let mut harness = long_user_message_harness(egui::vec2(430.0, 760.0));
        harness.run_ok();
        let message = harness.get_by_label(LONG_USER_MESSAGE).rect();
        assert!(message.left() >= 0.0 && message.right() <= 430.0);
        assert!(message.height() > 60.0);
        harness
            .render()
            .expect("render compact long user message")
            .save("/tmp/chatty-compact-long-user-message.png")
            .expect("save compact long user message");
    }

    fn notice_harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ChattyApp> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.error = Some(UiNotice {
                    timestamp: "2026-08-24 14:32:07 UTC".into(),
                    message: "The broker could not complete that request.".into(),
                });
                app
            })
    }

    fn restoring_harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ChattyApp> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.connected = true;
                app.restoring_session = true;
                app.status = "Restoring saved session…".into();
                app
            })
    }

    fn public_character_harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ChattyApp> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.characters[0].owned_by_user = false;
                app.draft = DraftCharacter::from(&app.characters[0]);
                app.draft_character_open = true;
                app
            })
    }

    fn add_overflowing_conversations(app: &mut ChattyApp) {
        for index in 1..=30 {
            app.conversations.push(Conversation {
                id: format!("overflow-{index}"),
                title: format!("Conversation {index:02}"),
                kind: ConversationKind::Direct,
                participant_ids: vec!["assistant".into()],
                state: String::new(),
                summary: String::new(),
                revision: index,
            });
        }
    }

    #[test]
    fn missing_conversation_is_reconciled_without_an_error_notice() {
        let (commands, mut requests) = mpsc::unbounded_channel();
        let (_, events) = std::sync::mpsc::channel();
        let mut app = ChattyApp::new(commands, events);
        app.load_inspection_demo();
        let available_conversations = app.conversations.clone();

        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 7,
            payload: encode(&Response::ConversationNotFound {
                conversation_id: "demo".into(),
            })
            .unwrap(),
        });

        assert!(app.error.is_none());
        assert!(app.selected_conversation.is_none());
        assert!(app.messages.is_empty());
        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 8,
            payload: encode(&Response::Conversations(available_conversations)).unwrap(),
        });
        assert!(app.selected_conversation.is_none());
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Request(request) if matches!(*request, Request::ListCharacters { .. })
        ));
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Request(request) if matches!(*request, Request::ListConversations { .. })
        ));
    }

    #[test]
    fn group_conversations_are_hidden_from_the_gui() {
        let (commands, mut requests) = mpsc::unbounded_channel();
        let (_, events) = std::sync::mpsc::channel();
        let mut app = ChattyApp::new(commands, events);
        app.token = "session".into();
        app.selected_conversation = Some("group".into());
        let conversation = |id: &str, kind| Conversation {
            id: id.into(),
            title: id.into(),
            kind,
            participant_ids: vec!["assistant".into()],
            state: String::new(),
            summary: String::new(),
            revision: 1,
        };

        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 10,
            payload: encode(&Response::Conversations(vec![
                conversation("group", ConversationKind::GroupManual),
                conversation("direct", ConversationKind::Direct),
            ]))
            .unwrap(),
        });

        assert_eq!(app.conversations.len(), 1);
        assert_eq!(app.conversations[0].id, "direct");
        assert_eq!(app.selected_conversation.as_deref(), Some("direct"));
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Request(request)
                if matches!(
                    *request,
                    Request::GetConversation { ref conversation_id, .. }
                        if conversation_id == "direct"
                )
        ));
    }

    #[test]
    fn expired_saved_session_returns_to_login() {
        let (commands, _) = mpsc::unbounded_channel();
        let (event_tx, events) = std::sync::mpsc::channel();
        let mut app = ChattyApp::new(commands, events);
        app.restoring_session = true;
        event_tx.send(Event::SessionExpired).unwrap();

        app.drain(&egui::Context::default());

        assert!(!app.restoring_session);
        assert!(app.token.is_empty());
        assert!(app.error.is_some());
    }

    #[test]
    fn logout_acknowledgement_does_not_refresh_with_an_empty_token() {
        let (commands, mut requests) = mpsc::unbounded_channel();
        let (_, events) = std::sync::mpsc::channel();
        let mut app = ChattyApp::new(commands, events);

        app.handle_frame(Frame {
            compressed: false,
            message_type: MessageType::Response,
            request_id: 9,
            payload: encode(&Response::Accepted {
                entity_id: None,
                revision: 0,
            })
            .unwrap(),
        });

        assert!(requests.try_recv().is_err());
        assert!(app.error.is_none());
    }

    #[test]
    fn visual_desktop_restoring_session() {
        let mut restoring = restoring_harness(egui::vec2(1440.0, 900.0));
        restoring.get_by_label("Signing you in");
        restoring
            .render()
            .expect("render desktop session restore")
            .save("/tmp/chatty-restoring-session-desktop.png")
            .expect("save desktop session restore");
    }

    #[test]
    fn visual_compact_restoring_session() {
        let mut restoring = restoring_harness(egui::vec2(430.0, 760.0));
        restoring.get_by_label("Signing you in");
        restoring
            .render()
            .expect("render compact session restore")
            .save("/tmp/chatty-restoring-session-compact.png")
            .expect("save compact session restore");
    }

    #[test]
    fn visual_desktop_chat() {
        harness(egui::vec2(1440.0, 900.0))
            .render()
            .expect("render desktop UI")
            .save("/tmp/chatty-restored-desktop.png")
            .expect("save desktop UI");
    }

    #[test]
    fn visual_narrow_split_chat() {
        harness(egui::vec2(900.0, 650.0))
            .render()
            .expect("render narrow split UI")
            .save("/tmp/chatty-restored-narrow-split.png")
            .expect("save narrow split UI");
    }

    #[test]
    fn visual_desktop_overflowing_conversation_list() {
        let mut harness = harness(egui::vec2(1440.0, 900.0));
        add_overflowing_conversations(harness.state_mut());
        harness.run_ok();
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "Conversation 30")
            .scroll_to_me();
        harness.run_ok();
        let last = harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "Conversation 30")
            .rect();
        let footer = harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "Manage characters")
            .rect();
        assert!(last.bottom() <= footer.top());
        harness
            .render()
            .expect("render desktop overflowing conversation list")
            .save("/tmp/chatty-overflowing-conversations-desktop.png")
            .expect("save desktop overflowing conversation list");
    }

    #[test]
    fn visual_compact_overflowing_conversation_list() {
        let mut harness = harness(egui::vec2(430.0, 760.0));
        add_overflowing_conversations(harness.state_mut());
        harness.run_ok();
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "Conversation 30")
            .scroll_to_me();
        harness.run_ok();
        let last = harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "Conversation 30")
            .rect();
        let footer = harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "Manage characters")
            .rect();
        assert!(last.bottom() <= footer.top());
        harness
            .render()
            .expect("render compact overflowing conversation list")
            .save("/tmp/chatty-overflowing-conversations-compact.png")
            .expect("save compact overflowing conversation list");
    }

    #[test]
    fn visual_minimum_height_sidebar() {
        let mut harness = harness(egui::vec2(640.0, 480.0));
        add_overflowing_conversations(harness.state_mut());
        harness.run_ok();
        harness
            .render()
            .expect("render minimum-height sidebar")
            .save("/tmp/chatty-sidebar-minimum-height.png")
            .expect("save minimum-height sidebar");
    }

    #[test]
    fn visual_narrow_minimum_height_sidebar() {
        let mut harness = harness(egui::vec2(320.0, 480.0));
        add_overflowing_conversations(harness.state_mut());
        harness.run_ok();
        harness
            .render()
            .expect("render narrow minimum-height sidebar")
            .save("/tmp/chatty-sidebar-narrow-minimum-height.png")
            .expect("save narrow minimum-height sidebar");
    }

    #[test]
    fn visual_hover_chat_delete() {
        let mut harness = harness(egui::vec2(1440.0, 900.0));
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "A quiet evening")
            .hover();
        harness.run_ok();
        harness
            .render()
            .expect("render hovered delete control")
            .save("/tmp/chatty-restored-hover-delete.png")
            .expect("save hovered delete control");
    }

    #[test]
    fn visual_hover_message_delete() {
        let mut harness = harness(egui::vec2(1440.0, 900.0));
        harness.get_by_label("Tell me about this place.").hover();
        harness.run_ok();
        harness
            .render()
            .expect("render hovered message delete control")
            .save("/tmp/chatty-restored-hover-message-delete.png")
            .expect("save hovered message delete control");
    }

    #[test]
    fn visual_hover_assistant_message_actions() {
        let mut harness = harness(egui::vec2(1440.0, 900.0));
        harness.get_by_label("The old observatory").hover();
        harness.run_ok();
        harness.get_by_role_and_label(egui::accesskit::Role::Button, "↻");
        harness
            .render()
            .expect("render hovered assistant message actions")
            .save("/tmp/chatty-restored-hover-assistant-actions.png")
            .expect("save hovered assistant message actions");
    }

    #[test]
    fn visual_compact_chat() {
        let mut compact = egui_kittest::Harness::builder()
            .with_size(egui::vec2(430.0, 760.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.sidebar_visible = false;
                app
            });
        compact
            .render()
            .expect("render compact UI")
            .save("/tmp/chatty-restored-compact.png")
            .expect("save compact UI");
    }

    #[test]
    fn visual_compact_light_chat() {
        let mut compact = egui_kittest::Harness::builder()
            .with_size(egui::vec2(430.0, 760.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, true, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.light_mode = true;
                app.sidebar_visible = false;
                app
            });
        compact
            .render()
            .expect("render compact light UI")
            .save("/tmp/chatty-restored-compact-light.png")
            .expect("save compact light UI");
    }

    #[test]
    fn visual_desktop_empty_chat() {
        let mut empty = harness(egui::vec2(1440.0, 900.0));
        empty.state_mut().selected_conversation = None;
        empty.state_mut().messages.clear();
        empty.run_ok();
        empty
            .render()
            .expect("render desktop empty chat")
            .save("/tmp/chatty-empty-desktop.png")
            .expect("save desktop empty chat");
    }

    #[test]
    fn visual_compact_empty_chat() {
        let mut empty = harness(egui::vec2(430.0, 760.0));
        empty.state_mut().sidebar_visible = false;
        empty.state_mut().selected_conversation = None;
        empty.state_mut().messages.clear();
        empty.run_ok();
        empty
            .render()
            .expect("render compact empty chat")
            .save("/tmp/chatty-empty-compact.png")
            .expect("save compact empty chat");
    }

    #[test]
    fn visual_desktop_new_chat_dialog() {
        let mut new_chat = harness(egui::vec2(1440.0, 900.0));
        new_chat.state_mut().new_chat_open = true;
        new_chat.run_ok();
        new_chat.get_by_label("New chat");
        new_chat
            .render()
            .expect("render desktop new chat dialog")
            .save("/tmp/chatty-new-chat-desktop.png")
            .expect("save desktop new chat dialog");
    }

    #[test]
    fn creating_conversation_closes_new_chat_dialog() {
        let (commands, mut requests) = mpsc::unbounded_channel();
        let (_, events) = std::sync::mpsc::channel();
        let mut new_chat = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app
            });
        new_chat.state_mut().new_chat_open = true;
        new_chat
            .state_mut()
            .selected_characters
            .insert("assistant".into());
        new_chat.run_ok();

        new_chat
            .get_by_role_and_label(egui::accesskit::Role::Button, "Create conversation")
            .click();
        new_chat.run_ok();

        assert!(!new_chat.state().new_chat_open);
        assert!(new_chat.query_by_label("New chat").is_none());
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Request(request)
                if matches!(
                    *request,
                    Request::CreateConversation {
                        kind: ConversationKind::Direct,
                        ref participant_ids,
                        ..
                    } if participant_ids == &["assistant"]
                )
        ));
    }

    #[test]
    fn visual_compact_new_chat_dialog() {
        let mut new_chat = harness(egui::vec2(430.0, 760.0));
        new_chat.state_mut().sidebar_visible = false;
        new_chat.state_mut().new_chat_open = true;
        new_chat.run_ok();
        new_chat.get_by_label("New chat");
        new_chat
            .render()
            .expect("render compact new chat dialog")
            .save("/tmp/chatty-new-chat-compact.png")
            .expect("save compact new chat dialog");
    }

    #[test]
    fn visual_small_phone_new_chat_dialog() {
        let mut new_chat = harness(egui::vec2(375.0, 667.0));
        new_chat.state_mut().sidebar_visible = false;
        new_chat.state_mut().new_chat_open = true;
        new_chat.run_ok();
        new_chat.get_by_label("New chat");
        new_chat
            .render()
            .expect("render small-phone new chat dialog")
            .save("/tmp/chatty-new-chat-375.png")
            .expect("save small-phone new chat dialog");
    }

    #[test]
    fn visual_compact_landscape_chat() {
        let mut landscape = harness(egui::vec2(667.0, 375.0));
        landscape.state_mut().sidebar_visible = false;
        landscape.run_ok();
        landscape
            .render()
            .expect("render compact landscape chat")
            .save("/tmp/chatty-chat-landscape.png")
            .expect("save compact landscape chat");
    }

    #[test]
    fn visual_compact_hover_assistant_message_actions() {
        let mut compact = harness(egui::vec2(430.0, 760.0));
        compact.state_mut().sidebar_visible = false;
        compact.run_ok();
        compact.get_by_label("The old observatory").hover();
        compact.run_ok();
        compact.get_by_role_and_label(egui::accesskit::Role::Button, "↻");
        compact
            .render()
            .expect("render compact hovered assistant message actions")
            .save("/tmp/chatty-restored-compact-hover-assistant-actions.png")
            .expect("save compact hovered assistant message actions");
    }

    #[test]
    fn visual_minimum_chat() {
        let mut minimum = harness(egui::vec2(640.0, 480.0));
        minimum.state_mut().sidebar_visible = false;
        minimum.run_ok();
        minimum
            .render()
            .expect("render minimum-size UI")
            .save("/tmp/chatty-minimum-640.png")
            .expect("save minimum-size UI");
    }

    #[test]
    fn visual_minimum_login() {
        let mut login = egui_kittest::Harness::builder()
            .with_size(egui::vec2(640.0, 480.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                ChattyApp::new(commands, events)
            });
        login
            .render()
            .expect("render minimum-size login")
            .save("/tmp/chatty-minimum-login.png")
            .expect("save minimum-size login");
    }

    #[test]
    fn visual_8k_chat() {
        harness(egui::vec2(7680.0, 4320.0))
            .render()
            .expect("render 8K UI")
            .save("/tmp/chatty-8k.png")
            .expect("save 8K UI");
    }

    #[test]
    fn visual_desktop_timestamped_notice() {
        notice_harness(egui::vec2(1440.0, 900.0))
            .render()
            .expect("render desktop timestamped notice")
            .save("/tmp/chatty-desktop-timestamped-notice.png")
            .expect("save desktop timestamped notice");
    }

    #[test]
    fn visual_compact_timestamped_notice() {
        notice_harness(egui::vec2(430.0, 760.0))
            .render()
            .expect("render compact timestamped notice")
            .save("/tmp/chatty-compact-timestamped-notice.png")
            .expect("save compact timestamped notice");
    }

    #[test]
    fn visual_compact_character_popup() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(430.0, 760.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.draft = DraftCharacter::from(&app.characters[0]);
                let template = app.characters[0].clone();
                for index in 1..24 {
                    let mut character = template.clone();
                    character.id = format!("character-{index}");
                    character.name = format!("Character {index}");
                    app.characters.push(character);
                }
                app.draft_character_open = true;
                app
            });
        harness
            .render()
            .expect("render compact character popup")
            .save("/tmp/chatty-restored-compact-characters.png")
            .expect("save compact character popup");
    }

    #[test]
    fn visual_desktop_character_popup() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.draft = DraftCharacter::from(&app.characters[0]);
                let template = app.characters[0].clone();
                for index in 1..24 {
                    let mut character = template.clone();
                    character.id = format!("character-{index}");
                    character.name = format!("Character {index}");
                    app.characters.push(character);
                }
                app.draft_character_open = true;
                app
            });
        harness
            .render()
            .expect("render desktop character popup")
            .save("/tmp/chatty-restored-desktop-characters.png")
            .expect("save desktop character popup");
    }

    #[test]
    fn public_character_is_read_only_for_non_owner() {
        let mut harness = public_character_harness(egui::vec2(1440.0, 900.0));

        harness.run_ok();
        harness.get_by_label("View character");
        harness.get_by_label("Public character · Only the owner can edit it.");
        assert!(
            harness
                .query_by_role_and_label(egui::accesskit::Role::Button, "Save")
                .is_none()
        );
        let fields: Vec<_> = harness
            .get_all_by_role(egui::accesskit::Role::TextInput)
            .collect();
        assert!(!fields.is_empty());
        assert!(
            fields
                .iter()
                .all(|field| field.accesskit_node().is_disabled())
        );
        harness
            .render()
            .expect("render desktop public character")
            .save("/tmp/chatty-public-character-desktop.png")
            .expect("save desktop public character");

        public_character_harness(egui::vec2(430.0, 760.0))
            .render()
            .expect("render compact public character")
            .save("/tmp/chatty-public-character-compact.png")
            .expect("save compact public character");
    }

    #[test]
    fn visual_admin_monitoring() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.screen = Screen::Admin;
                app
            });
        harness
            .render()
            .expect("render admin monitoring UI")
            .save("/tmp/chatty-restored-admin.png")
            .expect("save admin UI");
    }

    #[test]
    fn visual_compact_admin_monitoring() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(430.0, 760.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.screen = Screen::Admin;
                app
            });
        harness
            .render()
            .expect("render compact admin monitoring UI")
            .save("/tmp/chatty-admin-broker-compact.png")
            .expect("save compact admin UI");
    }

    fn render_settings(
        size: egui::Vec2,
        light_mode: bool,
        glass_mode: bool,
        transparency: u8,
        path: &str,
    ) {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(move |creation| {
                configure_style_with_surface(
                    &creation.egui_ctx,
                    light_mode,
                    glass_mode,
                    transparency,
                );
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.light_mode = light_mode;
                app.glass_mode = glass_mode;
                app.transparency = transparency;
                app.screen = Screen::Settings;
                app
            });
        harness
            .render()
            .expect("render settings UI")
            .save(path)
            .expect("save settings UI");
    }

    #[test]
    fn visual_settings_desktop_dark() {
        render_settings(
            egui::vec2(1440.0, 900.0),
            false,
            false,
            20,
            "/tmp/chatty-settings-desktop-dark.png",
        );
    }

    #[test]
    fn visual_settings_compact_light() {
        render_settings(
            egui::vec2(430.0, 760.0),
            true,
            false,
            20,
            "/tmp/chatty-settings-compact-light.png",
        );
    }

    #[test]
    fn visual_settings_glass_dark_desktop() {
        render_settings(
            egui::vec2(1440.0, 900.0),
            false,
            true,
            80,
            "/tmp/chatty-settings-glass-dark-desktop.png",
        );
    }

    #[test]
    fn visual_settings_glass_light_compact() {
        render_settings(
            egui::vec2(430.0, 760.0),
            true,
            true,
            80,
            "/tmp/chatty-settings-glass-light-compact.png",
        );
    }

    #[test]
    fn visual_settings_short_viewport() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(320.0, 480.0))
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, true, 80);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.glass_mode = true;
                app.transparency = 80;
                app.screen = Screen::Settings;
                app
            });
        let appearance = harness.get_by_label("Appearance");
        appearance.scroll_down();
        appearance.scroll_down();
        harness.run_ok();
        let total = harness.get_by_label("Total").rect();
        assert!(total.top() >= 0.0 && total.bottom() <= 480.0);
        harness
            .render()
            .expect("render scrolled settings UI at short viewport")
            .save("/tmp/chatty-settings-short-scroll.png")
            .expect("save scrolled settings UI at short viewport");
    }

    #[test]
    fn visual_glass_surfaces_at_maximum_transparency() {
        for (size, light_mode, path) in [
            (
                egui::vec2(1440.0, 900.0),
                false,
                "/tmp/chatty-glass-80-dark-desktop.png",
            ),
            (
                egui::vec2(430.0, 760.0),
                true,
                "/tmp/chatty-glass-80-light-compact.png",
            ),
        ] {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(size)
                .build_eframe(move |creation| {
                    configure_style_with_surface(&creation.egui_ctx, light_mode, true, 80);
                    let (commands, _) = mpsc::unbounded_channel();
                    let (_, events) = std::sync::mpsc::channel();
                    let mut app = ChattyApp::new(commands, events);
                    app.load_inspection_demo();
                    app.light_mode = light_mode;
                    app.glass_mode = true;
                    app.transparency = 80;
                    app
                });
            harness
                .render()
                .expect("render maximum-transparency glass UI")
                .save(path)
                .expect("save maximum-transparency glass UI");
        }
    }

    #[test]
    fn visual_admin_users_desktop_and_compact() {
        for (size, path) in [
            (
                egui::vec2(1440.0, 900.0),
                "/tmp/chatty-admin-users-desktop.png",
            ),
            (
                egui::vec2(430.0, 760.0),
                "/tmp/chatty-admin-users-compact.png",
            ),
        ] {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(size)
                .build_eframe(|creation| {
                    configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                    let (commands, _) = mpsc::unbounded_channel();
                    let (_, events) = std::sync::mpsc::channel();
                    let mut app = ChattyApp::new(commands, events);
                    app.load_inspection_demo();
                    app.screen = Screen::Admin;
                    app.admin_tab = 1;
                    app
                });
            harness
                .render()
                .expect("render admin users UI")
                .save(path)
                .expect("save admin users UI");
        }
    }

    fn render_ollama_admin(size: egui::Vec2, path: &str) {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style_with_surface(&creation.egui_ctx, false, false, 20);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app.screen = Screen::Admin;
                app.admin_tab = 2;
                app
            });
        harness
            .render()
            .expect("render Ollama administration UI")
            .save(path)
            .expect("save Ollama administration UI");
    }

    #[test]
    fn visual_desktop_ollama_admin() {
        render_ollama_admin(
            egui::vec2(1440.0, 900.0),
            "/tmp/chatty-admin-ollama-desktop.png",
        );
    }

    #[test]
    fn visual_compact_ollama_admin() {
        render_ollama_admin(
            egui::vec2(430.0, 760.0),
            "/tmp/chatty-admin-ollama-compact.png",
        );
    }

    #[test]
    fn admin_monitor_refreshes_only_while_open() {
        let (commands, mut command_rx) = mpsc::unbounded_channel();
        let (_, events) = std::sync::mpsc::channel();
        let mut app = ChattyApp::new(commands, events);
        app.token = "session".into();
        app.screen = Screen::Admin;
        app.admin_tab = 0;
        app.last_monitor_refresh = Some(Instant::now() - Duration::from_secs(3));
        app.refresh_admin_monitor_if_due(&egui::Context::default());
        assert!(matches!(
            command_rx.try_recv(),
            Ok(Command::Request(request))
                if matches!(*request, Request::AdminGetBrokerMonitor { .. })
        ));

        app.screen = Screen::Chat;
        app.last_monitor_refresh = Some(Instant::now() - Duration::from_secs(3));
        app.refresh_admin_monitor_if_due(&egui::Context::default());
        assert!(command_rx.try_recv().is_err());
    }
}

impl Drop for ChattyApp {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
    }
}
impl eframe::App for ChattyApp {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain(&ctx);
        self.refresh_admin_monitor_if_due(&ctx);
        if self.glass_mode {
            paint_glass_background(ui, self.light_mode);
        }
        let edge_padding = if ui.available_width() < 760.0 { 8 } else { 12 };
        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(edge_padding)
            .show(ui, |ui| {
                if !self.connected {
                    self.render_server_connection(ui)
                } else if self.restoring_session {
                    self.render_session_restore(ui)
                } else if self.token.is_empty() {
                    self.render_login(ui)
                } else {
                    self.render_shell(ui)
                }
            });
        let modal_open = self.draft_character_open
            || self.new_chat_open
            || matches!(self.screen, Screen::Admin | Screen::Settings)
            || self.error.is_some();
        if self.glass_mode && modal_open {
            paint_glass_modal_scrim(ui, self.light_mode);
        }
        if self.draft_character_open {
            self.render_character_dialog(&ctx)
        }
        if self.new_chat_open {
            self.render_new_chat_dialog(&ctx)
        }
        if self.screen == Screen::Admin {
            self.render_admin_dialog(&ctx)
        }
        if self.screen == Screen::Settings {
            self.render_settings_dialog(&ctx)
        }
        if let Some(error) = self.error.clone() {
            egui::Window::new("Notice")
                .frame(modal_frame(&ctx, self.light_mode, self.glass_mode))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_TOP, [0.0, 28.0])
                .show(&ctx, |ui| {
                    ui.set_max_width(360.0);
                    ui.label(egui::RichText::new(error.timestamp).small().weak());
                    ui.label(error.message);
                    if ui.button("Close").clicked() {
                        self.error = None;
                    }
                });
        }
    }
}

impl ChattyApp {
    fn render_server_connection(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            let top = (ui.available_height() * 0.18).clamp(36.0, 150.0);
            ui.add_space(top);
            ui.heading(egui::RichText::new("Connect to Chatty").size(30.0));
            ui.label("Enter the IP address or domain of your Chatty server.");
            ui.add_space(22.0);
            ui.scope(|ui| {
                ui.set_max_width(420.0);
                ui.label("Server address");
                let response = ui.add_enabled(
                    !self.connecting,
                    egui::TextEdit::singleline(&mut self.server_address)
                        .hint_text("192.168.0.98 or chatty.example.com")
                        .desired_width(ui.available_width()),
                );
                let submit =
                    response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.add_space(10.0);
                let clicked = ui
                    .add_enabled(
                        !self.connecting,
                        egui::Button::new(if self.connecting {
                            "Trying connection…"
                        } else {
                            "Try connection"
                        })
                        .min_size(egui::vec2(ui.available_width(), 42.0))
                        .fill(egui::Color32::from_rgb(37, 99, 235)),
                    )
                    .clicked();
                if (clicked || submit) && !self.connecting {
                    match connection_target(&self.server_address) {
                        Some(target) => {
                            self.server_address = target.server_name.clone();
                            self.connecting = true;
                            self.status = format!("Connecting to {}: 7443…", self.server_address);
                            self.error = None;
                            let _ = self.commands.send(Command::Connect(target));
                        }
                        None => self.set_error("Enter a valid IP address or domain."),
                    }
                }
                ui.add_space(8.0);
                ui.label(egui::RichText::new(&self.status).small().weak());
            });
        });
    }

    fn render_session_restore(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            let top = (ui.available_height() * 0.28).clamp(72.0, 220.0);
            ui.add_space(top);
            ui.spinner();
            ui.add_space(14.0);
            ui.heading(egui::RichText::new("Signing you in").size(30.0));
            ui.add_space(6.0);
            ui.label(&self.status);
        });
    }

    fn render_login(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            let top = (ui.available_height() * 0.18).clamp(36.0, 150.0);
            ui.add_space(top);
            ui.heading(egui::RichText::new("Welcome back").size(30.0));
            ui.label(&self.status);
            ui.add_space(22.0);
            ui.scope(|ui| {
                ui.set_max_width(420.0);
                ui.label("Username");
                ui.add_sized(
                    [ui.available_width(), 42.0],
                    egui::TextEdit::singleline(&mut self.username),
                );
                ui.label("Password");
                let response = ui.add_sized(
                    [ui.available_width(), 42.0],
                    egui::TextEdit::singleline(&mut self.password).password(true),
                );
                let submit = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(10.0);
                if ui
                    .add_sized(
                        [ui.available_width(), 42.0],
                        egui::Button::new(egui::RichText::new("Sign in").strong())
                            .fill(egui::Color32::from_rgb(37, 99, 235)),
                    )
                    .clicked()
                    || submit
                {
                    self.send(Request::Login {
                        username: self.username.clone(),
                        password: self.password.clone(),
                    });
                    self.status = "Signing in…".into();
                }
                if self.registration_enabled
                    && ui
                        .add_sized([ui.available_width(), 38.0], egui::Button::new("Register"))
                        .clicked()
                {
                    self.send(Request::Register {
                        username: self.username.clone(),
                        password: self.password.clone(),
                    });
                    self.status = "Creating account…".into();
                }
                ui.add_space(8.0);
                if ui.button("Change server").clicked() {
                    let _ = self.commands.send(Command::Disconnect);
                    self.connected = false;
                    self.restoring_session = false;
                    self.status = "Enter the server IP to begin.".into();
                }
            });
        });
    }
    fn render_shell(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 760.0;
        if compact {
            if self.sidebar_visible {
                egui::Frame::new()
                    .fill(ui.visuals().window_fill)
                    .corner_radius(12.0)
                    .inner_margin(12.0)
                    .show(ui, |ui| self.render_sidebar(ui, true));
            } else {
                self.render_chat(ui);
            }
            return;
        }
        let sidebar = (ui.available_width() * 0.24).clamp(248.0, 320.0);
        let height = ui.available_height();
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(sidebar, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::Frame::new()
                        .fill(ui.visuals().window_fill)
                        .corner_radius(12.0)
                        .inner_margin(12.0)
                        .show(ui, |ui| self.render_sidebar(ui, compact));
                },
            );
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.render_chat(ui),
            );
        });
    }
    fn render_sidebar(&mut self, ui: &mut egui::Ui, compact: bool) {
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new("Chatty").size(22.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new("AI CHARACTERS").size(10.0).weak());
            });
        });
        ui.add_space(12.0);
        if ui
            .add_sized(
                [ui.available_width(), 42.0],
                egui::Button::new(egui::RichText::new("+  New chat").strong())
                    .fill(COLOR_PRIMARY_STRONG)
                    .corner_radius(10.0),
            )
            .clicked()
        {
            self.new_chat_open = true;
        }
        ui.add_space(14.0);
        ui.label(egui::RichText::new("RECENT CHATS").size(11.0).weak());
        ui.add_space(2.0);
        let mut remaining = ui.available_rect_before_wrap();
        remaining.max.y = (remaining.max.y - 12.0).max(remaining.min.y);
        let footer_height = 90.0_f32.min(remaining.height());
        let footer_rect = egui::Rect::from_min_max(
            egui::pos2(remaining.left(), remaining.bottom() - footer_height),
            remaining.max,
        );
        let list_rect = egui::Rect::from_min_max(
            remaining.min,
            egui::pos2(
                remaining.right(),
                (footer_rect.top() - 4.0).max(remaining.top()),
            ),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(footer_rect), |ui| {
            ui.add_space(4.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&self.status).size(12.0).weak());
            });
            ui.horizontal(|ui| {
                if Self::footer_icon_button(ui, FooterIcon::Characters, "Manage characters")
                    .clicked()
                {
                    self.screen = Screen::Characters;
                    self.draft_character_open = true;
                }
                if self.role == Some(Role::Admin)
                    && Self::footer_icon_button(ui, FooterIcon::Admin, "Open admin portal")
                        .clicked()
                {
                    self.screen = Screen::Admin;
                    self.admin_tab = 0;
                    self.load_admin_tab();
                }
                if Self::footer_icon_button(ui, FooterIcon::Settings, "Open settings").clicked() {
                    self.screen = Screen::Settings;
                    self.send(Request::GetAccountUsage {
                        session_token: self.token.clone(),
                    });
                }
                if Self::footer_icon_button(ui, FooterIcon::SignOut, "Sign out").clicked() {
                    self.send(Request::Logout {
                        session_token: self.token.clone(),
                    });
                    let _ = self.commands.send(Command::ClearSession);
                    self.token.clear();
                    self.role = None;
                }
            });
        });
        ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("conversation-list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for c in self.conversations.clone() {
                        ui.push_id(&c.id, |ui| {
                            let selected = self.selected_conversation.as_deref() == Some(&c.id);
                            let (row_rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 44.0),
                                egui::Sense::hover(),
                            );
                            let tile_rect = row_rect.shrink2(egui::vec2(0.0, 2.0));
                            let tile =
                                ui.put(tile_rect, egui::Button::selectable(selected, &c.title));
                            if tile.clicked() {
                                self.open_conversation(&c.id);
                                if compact {
                                    self.sidebar_visible = false;
                                }
                            }
                            let hovered = ui
                                .input(|input| input.pointer.hover_pos())
                                .is_some_and(|pointer| tile_rect.contains(pointer));
                            let delete_rect = egui::Rect::from_min_size(
                                egui::pos2(tile_rect.right() - 28.0, tile_rect.top() + 4.0),
                                egui::vec2(24.0, 28.0),
                            );
                            if hovered
                                && ui
                                    .put(delete_rect, egui::Button::new("×").frame(false))
                                    .on_hover_text("Delete")
                                    .clicked()
                            {
                                self.send(Request::DeleteEntity {
                                    session_token: self.token.clone(),
                                    kind: EntityKind::Conversation,
                                    entity_id: c.id.clone(),
                                });
                            }
                        });
                    }
                });
        });
    }

    fn render_settings_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.screen == Screen::Settings;
        let max_height = Self::popup_max_height(ctx);
        let dialog_width = (ctx.content_rect().width() - 24.0).clamp(280.0, 420.0);
        egui::Window::new("Settings")
            .frame(modal_frame(ctx, self.light_mode, self.glass_mode))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_size([dialog_width, max_height.min(560.0)])
            .max_width(dialog_width)
            .max_height(max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("settings-popup-content")
                    .max_height((max_height - 32.0).max(120.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.heading("Appearance");
                        ui.label("Choose how Chatty looks on this device.");
                        ui.add_space(6.0);
                        ui.weak("Theme");
                        let previous_light_mode = self.light_mode;
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(
                                    [96.0, 40.0],
                                    egui::Button::selectable(!self.light_mode, "Dark"),
                                )
                                .clicked()
                            {
                                self.light_mode = false;
                            }
                            if ui
                                .add_sized(
                                    [96.0, 40.0],
                                    egui::Button::selectable(self.light_mode, "Light"),
                                )
                                .clicked()
                            {
                                self.light_mode = true;
                            }
                        });
                        ui.add_space(8.0);
                        ui.weak("Surface");
                        let previous_glass_mode = self.glass_mode;
                        let previous_transparency = self.transparency;
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(
                                    [96.0, 40.0],
                                    egui::Button::selectable(!self.glass_mode, "Solid"),
                                )
                                .clicked()
                            {
                                self.glass_mode = false;
                            }
                            if ui
                                .add_sized(
                                    [96.0, 40.0],
                                    egui::Button::selectable(self.glass_mode, "Glass"),
                                )
                                .clicked()
                            {
                                self.glass_mode = true;
                            }
                        });
                        ui.add_space(8.0);
                        ui.add_enabled_ui(self.glass_mode, |ui| {
                            ui.add_sized(
                                [ui.available_width().min(320.0), 40.0],
                                egui::Slider::new(&mut self.transparency, 0..=80)
                                    .text("Transparency")
                                    .suffix("%"),
                            );
                            ui.weak("0% is opaque · 80% is most transparent");
                        });
                        if previous_light_mode != self.light_mode
                            || previous_glass_mode != self.glass_mode
                            || previous_transparency != self.transparency
                        {
                            configure_style_with_surface(
                                ctx,
                                self.light_mode,
                                self.glass_mode,
                                self.transparency,
                            );
                            if let Some(path) = &self.preferences_path {
                                network::save_preferences(
                                    path,
                                    self.light_mode,
                                    self.glass_mode,
                                    self.transparency,
                                );
                            }
                            ctx.request_repaint();
                        }
                        ui.add_space(18.0);
                        ui.separator();
                        ui.heading("Token usage");
                        egui::Grid::new("account-token-usage")
                            .num_columns(2)
                            .spacing([24.0, 8.0])
                            .show(ui, |ui| {
                                ui.label("Prompt tokens");
                                ui.strong(format_token_count(self.account_usage.prompt_tokens));
                                ui.end_row();
                                ui.label("Completion tokens");
                                ui.strong(format_token_count(self.account_usage.completion_tokens));
                                ui.end_row();
                                ui.label("Total");
                                ui.strong(format_token_count(self.account_usage.total()));
                                ui.end_row();
                            });
                    });
            });
        if !open {
            self.screen = Screen::Chat;
        }
    }
}

fn format_token_count(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}
