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
use network::{Command, Event};

#[derive(Parser, Clone)]
struct Args {
    #[arg(long, env = "CHATTY_BROKER", default_value = "127.0.0.1:7443")]
    broker: String,
    #[arg(long, env = "CHATTY_SERVER_NAME", default_value = "localhost")]
    server_name: String,
    #[arg(long, env = "CHATTY_CA", default_value = "certs/ca.pem")]
    ca: String,
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
    if !inspect {
        // Resolve this before starting eframe so debug and release launches use
        // the same absolute XDG state path and never fall back to the cwd.
        args.session_file = Some(network::session_path(&args)?);
    }
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
                    .block_on(network::run(args, command_rx, event_tx))
            })?;
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([360.0, 520.0])
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native(
        "Chatty",
        options,
        Box::new(move |cc| {
            configure_style(&cc.egui_ctx);
            let mut app = ChattyApp::new(command_tx, event_rx);
            if inspect {
                app.load_inspection_demo();
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals {
        panel_fill: egui::Color32::from_rgba_unmultiplied(13, 15, 20, 238),
        window_fill: egui::Color32::from_rgba_unmultiplied(20, 23, 30, 242),
        extreme_bg_color: egui::Color32::from_rgba_unmultiplied(8, 10, 14, 210),
        ..egui::Visuals::dark()
    });
    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    ctx.set_style_of(egui::Theme::Dark, style);
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
}

struct ChattyApp {
    commands: mpsc::UnboundedSender<Command>,
    events: std::sync::mpsc::Receiver<Event>,
    status: String,
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
    new_conversation_kind: ConversationKind,
    users: Vec<UserAccount>,
    broker_config: BrokerConfig,
    broker_monitor: Option<BrokerMonitor>,
    admin_data: Vec<AdminDataRow>,
    admin_tab: usize,
    admin_new_username: String,
    admin_new_password: String,
    admin_new_role: Role,
    last_monitor_refresh: Option<Instant>,
}

impl ChattyApp {
    fn new(
        commands: mpsc::UnboundedSender<Command>,
        events: std::sync::mpsc::Receiver<Event>,
    ) -> Self {
        Self {
            commands,
            events,
            status: "Starting…".into(),
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
            new_conversation_kind: ConversationKind::Direct,
            users: vec![],
            broker_config: BrokerConfig {
                adapter_enabled: false,
                adapter_url: String::new(),
                allow_public_characters: false,
                allow_self_registration: true,
            },
            broker_monitor: None,
            admin_data: vec![],
            admin_tab: 0,
            admin_new_username: String::new(),
            admin_new_password: String::new(),
            admin_new_role: Role::User,
            last_monitor_refresh: None,
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
        self.token = token;
        self.user_id = user_id;
        self.role = Some(role);
        self.revision = revision;
        self.status = "Online · TLS 1.3".into();
        self.password.clear();
        self.refresh();
    }
    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Status(s) => self.status = s,
                Event::SessionExpired => {
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
                            self.conversations = v;
                            if self.selected_conversation.is_none() {
                                self.selected_conversation =
                                    self.conversations.first().map(|c| c.id.clone());
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
                        Response::Users(v) => self.users = v,
                        Response::BrokerConfig(v) => self.broker_config = v,
                        Response::BrokerMonitor(v) => self.broker_monitor = Some(v),
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
                            if let Some(id) = self.selected_conversation.clone() {
                                self.open_conversation(&id)
                            }
                        }
                        Response::Accepted { revision, .. }
                        | Response::SyncComplete { revision } => {
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
        self.selected_conversation = Some(id.into());
        self.send(Request::GetConversation {
            session_token: self.token.clone(),
            conversation_id: id.into(),
        });
    }
    fn load_inspection_demo(&mut self) {
        self.status = "Online · inspection".into();
        self.token = "inspection".into();
        self.user_id = "admin".into();
        self.username = "admin".into();
        self.role = Some(Role::Admin);
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
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod visual_tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ChattyApp> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style(&creation.egui_ctx);
                let (commands, _) = mpsc::unbounded_channel();
                let (_, events) = std::sync::mpsc::channel();
                let mut app = ChattyApp::new(commands, events);
                app.load_inspection_demo();
                app
            })
    }

    fn notice_harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ChattyApp> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_eframe(|creation| {
                configure_style(&creation.egui_ctx);
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
    fn visual_hover_chat_delete() {
        let mut harness = harness(egui::vec2(1440.0, 900.0));
        harness.get_by_label("A quiet evening").hover();
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
    fn visual_compact_chat() {
        let mut compact = egui_kittest::Harness::builder()
            .with_size(egui::vec2(430.0, 760.0))
            .build_eframe(|creation| {
                configure_style(&creation.egui_ctx);
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
                configure_style(&creation.egui_ctx);
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
                configure_style(&creation.egui_ctx);
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
    fn visual_admin_monitoring() {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 760.0))
            .build_eframe(|creation| {
                configure_style(&creation.egui_ctx);
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
        let edge_padding = if ui.available_width() < 720.0 { 8 } else { 12 };
        egui::Frame::new()
            .inner_margin(edge_padding)
            .show(ui, |ui| {
                if self.token.is_empty() {
                    self.render_login(ui)
                } else {
                    self.render_shell(ui)
                }
            });
        if self.draft_character_open {
            self.render_character_dialog(&ctx)
        }
        if self.new_chat_open {
            self.render_new_chat_dialog(&ctx)
        }
        if self.screen == Screen::Admin {
            self.render_admin_dialog(&ctx)
        }
        if let Some(error) = self.error.clone() {
            egui::Window::new("Notice")
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
                    .add_sized([ui.available_width(), 42.0], egui::Button::new("Sign in"))
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
            });
        });
    }
    fn render_shell(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 720.0;
        if compact {
            if self.sidebar_visible {
                self.render_sidebar(ui, true);
            } else {
                self.render_chat(ui);
            }
            return;
        }
        let sidebar = (ui.available_width() * 0.27).clamp(220.0, 320.0);
        let height = ui.available_height();
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(sidebar, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.render_sidebar(ui, compact),
            );
            ui.separator();
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.render_chat(ui),
            );
        });
    }
    fn render_sidebar(&mut self, ui: &mut egui::Ui, compact: bool) {
        ui.set_min_height(ui.available_height());
        ui.add_space(8.0);
        if ui
            .add_sized([ui.available_width(), 40.0], egui::Button::new("New chat"))
            .clicked()
        {
            self.new_chat_open = true;
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for c in self.conversations.clone() {
                ui.push_id(&c.id, |ui| {
                    let selected = self.selected_conversation.as_deref() == Some(&c.id);
                    let (row_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 40.0),
                        egui::Sense::hover(),
                    );
                    let tile_rect = row_rect.shrink2(egui::vec2(6.0, 2.0));
                    let tile = ui.put(tile_rect, egui::Button::selectable(selected, &c.title));
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
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                if ui.button("Characters").clicked() {
                    self.screen = Screen::Characters;
                    self.draft_character_open = true;
                }
                if self.role == Some(Role::Admin) && ui.button("Admin").clicked() {
                    self.screen = Screen::Admin;
                    self.admin_tab = 0;
                    self.load_admin_tab();
                }
                if ui.button("Out").clicked() {
                    self.send(Request::Logout {
                        session_token: self.token.clone(),
                    });
                    let _ = self.commands.send(Command::ClearSession);
                    self.token.clear();
                    self.role = None;
                }
            });
        });
    }
}
