//! The application root: state, update dispatch, and views.

mod character_editor;
mod events;
mod inspect;
mod message;
mod subscription;
#[cfg(test)]
mod tests;
mod update;
mod view;

pub(crate) use message::Message;

use crate::admin::AdminState;
use crate::characters::{DraftCharacter, DraftEditors, ImageGeneration};
use crate::memories::MemoryDialogState;
use crate::network::{Command, Event, PendingCertificate, session_path};
use crate::tts::{LocalTts, TtsEvent, tts_channel};
use crate::worlds::WorldDialogState;
use crate::{Args, characters, conversation, network, ui};
use chatty_protocol::util::current_utc_timestamp;
use chatty_protocol::{
    Character, ChatMessage, Conversation, DeltaPayload, GenerationContext, GenerationStatus,
    MemoryEntry, Request, Role, TokenUsage, World,
};
use iced::{Task, Theme};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use subscription::ChannelHolder;
use tokio::sync::mpsc;

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
    image_generation: bool,
    scene_image: ImageJob,
    portrait_image: ImageJob,
    emotion_batch: Option<EmotionBatch>,
    message_images: HashMap<String, Vec<u8>>,
    message_image_requests: HashSet<String>,
    state: HashMap<(String, String), DeltaPayload>,
    characters: Vec<Character>,
    character_image_requests: HashSet<(String, String)>,
    character_search: String,
    worlds: Vec<World>,
    world_state: WorldDialogState,
    worlds_open: bool,
    memories: Vec<MemoryEntry>,
    memory_state: MemoryDialogState,
    memory_dialog_open: bool,
    conversations: Vec<Conversation>,
    messages: Vec<ChatMessage>,
    markdown: conversation::ConversationMarkdown,
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
    context_usage: HashMap<String, GenerationContext>,
    active_request: Option<u64>,
    screen: Screen,
    sidebar_visible: bool,
    compact_sidebar_open: bool,
    error: Option<UiNotice>,
    draft: DraftCharacter,
    draft_editors: DraftEditors,
    draft_world_ids: HashSet<String>,
    draft_character_open: bool,
    char_import_pending: bool,
    char_import_started: Option<Instant>,
    new_chat_open: bool,
    new_conversation_title: String,
    admin: AdminState,
    last_monitor_refresh: Option<Instant>,
    account_usage: TokenUsage,
    glass_mode: bool,
    preferences_path: Option<PathBuf>,
    tts: LocalTts,
    tts_auto_speak: bool,
    pending_tts_message_id: Option<String>,
}

/// Most recent image messages whose PNG bytes stay loaded for the open chat.
const MAX_LOADED_MESSAGE_IMAGES: usize = 12;

/// A broker image job. The request ID arrives after the request is written.
#[derive(Debug, Default, Clone, Copy)]
struct ImageJob {
    pending: bool,
    request_id: Option<u64>,
}

impl ImageJob {
    pub(super) fn state(self, available: bool) -> ImageGeneration {
        if self.pending {
            ImageGeneration::Pending
        } else if available {
            ImageGeneration::Ready
        } else {
            ImageGeneration::Unavailable
        }
    }
    pub(super) fn owns(self, request_id: u64) -> bool {
        self.pending && self.request_id == Some(request_id)
    }
}

/// A running emotion set. Every image shares one seed so the face stays consistent.
#[derive(Debug)]
struct EmotionBatch {
    seed: u64,
    /// Label of the image currently being generated.
    current: Option<&'static str>,
    queue: VecDeque<(&'static str, &'static str)>,
    done: usize,
    total: usize,
}

impl ChattyApp {
    pub(super) fn new(
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
            image_generation: false,
            scene_image: ImageJob::default(),
            portrait_image: ImageJob::default(),
            emotion_batch: None,
            message_images: HashMap::new(),
            message_image_requests: HashSet::new(),
            state: HashMap::new(),
            characters: Vec::new(),
            character_image_requests: HashSet::new(),
            character_search: String::new(),
            worlds: Vec::new(),
            world_state: WorldDialogState::default(),
            worlds_open: false,
            memories: Vec::new(),
            memory_state: MemoryDialogState::default(),
            memory_dialog_open: false,
            conversations: Vec::new(),
            messages: Vec::new(),
            markdown: conversation::ConversationMarkdown::default(),
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
            context_usage: HashMap::new(),
            active_request: None,
            screen: Screen::Login,
            sidebar_visible: true,
            compact_sidebar_open: false,
            error: None,
            draft: DraftCharacter::default(),
            draft_editors: DraftEditors::default(),
            draft_world_ids: HashSet::new(),
            draft_character_open: false,
            char_import_pending: false,
            char_import_started: None,
            new_chat_open: false,
            new_conversation_title: String::new(),
            admin: AdminState::default(),
            last_monitor_refresh: None,
            account_usage: TokenUsage::default(),
            glass_mode: true,
            preferences_path: None,
            tts,
            tts_auto_speak: false,
            pending_tts_message_id: None,
        }
    }

    pub(crate) fn boot(args: Args) -> (Self, Task<Message>) {
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let (event_sender, event_rx) = mpsc::unbounded_channel();

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
        app.admin.glass_mode = true;

        if args.inspect {
            app.load_inspection_demo();
            app.screen = Screen::Chat;
            app.connected = true;
        }

        (app, Task::none())
    }

    pub(crate) fn update(&mut self, message: Message) -> Task<Message> {
        let task = self.update_message(message);
        if self.token.is_empty() {
            characters::clear_image_handles();
            self.markdown = conversation::ConversationMarkdown::default();
        } else {
            self.markdown.update(&self.messages, &self.stream_text);
        }
        task
    }

    pub(crate) fn theme(&self) -> Theme {
        ui::theme()
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        self.zoom
    }

    pub(super) fn send(&self, request: Request) {
        let _ = self.commands.send(Command::Request(Box::new(request)));
    }

    pub(super) fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some(UiNotice {
            timestamp: current_utc_timestamp(),
            message: message.into(),
        });
    }
}
