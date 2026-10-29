//! Message dispatch and the feature-level update handlers.

use crate::admin;
use crate::admin::AdminMessage;
use crate::app::{ChattyApp, Message, Screen};
use crate::characters::DraftCharacter;
use crate::conversation::ConversationMessage;
use crate::memories::{MemoryAction, MemoryDialogState, MemoryMessage};
use crate::network::{Command, connection_target, save_preferences};
use crate::tts::TtsEvent;
use crate::worlds::{WorldAction, WorldMessage};
use chatty_protocol::{ChatMessage, ConversationKind, EntityKind, Request};
use iced::Task;
use std::time::{Duration, Instant};

impl ChattyApp {
    pub(super) fn update_message(&mut self, message: Message) -> Task<Message> {
        match message {
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
                self.set_draft(DraftCharacter::default());
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
                self.speech.auto_speak = speak;
                self.save_preferences();
                Task::none()
            }
            Message::SetTtsDialogueOnly(dialogue_only) => {
                self.speech.dialogue_only = dialogue_only;
                self.save_preferences();
                Task::none()
            }
            Message::TestVoice => {
                if let Err(error) = self
                    .tts
                    .speak("Chatty local speech synthesis is working properly.", true)
                {
                    self.set_error(error);
                }
                Task::none()
            }
            Message::SignOut => {
                self.cancel_model_downloads();
                self.send(Request::Logout {
                    session_token: self.token.clone(),
                });
                let _ = self.commands.send(Command::ClearSession);
                self.clear_account();
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
            Message::InspectControl(cmd) => self.inspect_control(&cmd),
            Message::Character(msg) => self.update_character(msg),
            Message::World(msg) => {
                self.update_world(msg);
                Task::none()
            }
            Message::Memory(msg) => self.update_memory(msg),
            Message::Conversation(msg) => self.update_conversation(msg),
            Message::Admin(msg) => {
                self.update_admin(msg);
                Task::none()
            }
            Message::Tts(event) => {
                self.update_tts(event);
                Task::none()
            }
            Message::Network(event) => {
                self.handle_event(event);
                Task::none()
            }
        }
    }

    /// Forget the in-flight generation and its typing indicator.
    pub(super) fn end_generation(&mut self) {
        self.active_request = None;
        self.typing_character = None;
        self.generation_status = None;
    }

    /// Leave the open conversation, dropping its transcript.
    pub(super) fn close_conversation(&mut self) {
        self.selected_conversation = None;
        self.messages.clear();
        self.stream_text.clear();
        self.end_generation();
    }

    pub(super) fn finish_generation(&mut self, revision: i64) {
        self.revision = self.revision.max(revision);
        self.end_generation();
        self.send(Request::GetAccountUsage {
            session_token: self.token.clone(),
        });
        if let Some(id) = self.selected_conversation.clone() {
            self.open_conversation(&id);
        }
    }

    pub(super) fn open_conversation(&mut self, id: &str) {
        self.auto_select_conversation = true;
        self.selected_conversation = Some(id.into());
        self.send(Request::GetConversation {
            session_token: self.token.clone(),
            conversation_id: id.into(),
        });
    }

    pub(super) fn submit_message(&mut self) {
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

    pub(super) fn regenerate(&self, message: &ChatMessage) {
        if let Some(cid) = self.selected_conversation.clone() {
            self.send(Request::Generate {
                session_token: self.token.clone(),
                conversation_id: cid,
                speaker_id: message.author_id.clone(),
                parent_id: Some(message.id.clone()),
            });
        }
    }

    pub(super) fn selected_speaker(&self) -> Option<&str> {
        self.selected_conversation
            .as_deref()
            .and_then(|id| self.conversations.iter().find(|c| c.id == id))
            .and_then(|c| c.participant_ids.first().map(String::as_str))
    }

    pub(super) fn open_memory_manager(
        &mut self,
        character_id: Option<String>,
        conversation_id: Option<String>,
    ) {
        self.memory_state = MemoryDialogState::with_filters(character_id, conversation_id);
        self.memory_dialog_open = true;
        self.request_filtered_memories();
    }

    pub(super) fn request_filtered_memories(&self) {
        self.send(Request::ListMemories {
            session_token: self.token.clone(),
            conversation_id: self.memory_state.filter_conversation_id.clone(),
            character_id: self.memory_state.filter_character_id.clone(),
        });
    }

    pub(super) fn load_admin_tab(&mut self) {
        if self.admin.admin_tab == 2 && self.admin.models.busy {
            return;
        }
        for req in admin::requests_for_tab(self.admin.admin_tab, &self.token) {
            self.send(req);
        }
        if self.admin.admin_tab == 0 {
            self.last_monitor_refresh = Some(Instant::now());
        }
    }

    pub(super) fn refresh_admin_monitor_if_due(&mut self) {
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

    /// Female voice for characters whose gender field says so.
    pub(super) fn speaker_is_female(&self, author_id: Option<&str>) -> bool {
        author_id
            .and_then(|id| self.characters.iter().find(|character| character.id == id))
            .is_some_and(|character| crate::tts::is_female(&character.gender))
    }

    /// Text read aloud for a reply, honouring the dialogue-only preference.
    pub(super) fn spoken_text(&self, text: &str) -> String {
        if self.speech.dialogue_only {
            crate::tts::dialogue(text)
        } else {
            text.to_owned()
        }
    }

    pub(super) fn save_preferences(&self) {
        if let Some(path) = &self.preferences_path {
            save_preferences(path, self.speech);
        }
    }

    pub(super) fn update_world(&mut self, msg: WorldMessage) {
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
    }

    pub(super) fn update_memory(&mut self, msg: MemoryMessage) -> Task<Message> {
        if let MemoryMessage::Edit(action) = msg {
            return self.memory_state.perform(action).map(Message::Memory);
        }
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
            MemoryAction::Refresh => {
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

    pub(super) fn update_admin(&mut self, msg: AdminMessage) {
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
    }

    pub(super) fn update_tts(&mut self, event: TtsEvent) {
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
    }

    pub(super) fn update_conversation(&mut self, msg: ConversationMessage) -> Task<Message> {
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
                // A reply without quoted dialogue is read in full on request.
                let spoken = self.spoken_text(&text);
                let spoken = if spoken.trim().is_empty() {
                    text
                } else {
                    spoken
                };
                let female = self.speaker_is_female(author_id.as_deref());
                if let Err(e) = self.tts.speak(&spoken, female) {
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
            ConversationMessage::CopyText(text) => {
                return iced::clipboard::write(text);
            }
            ConversationMessage::GenerateSceneImage => {
                if let Some(conversation_id) = self.selected_conversation.clone()
                    && self.image_generation
                    && !self.scene_image.pending
                {
                    self.scene_image.pending = true;
                    let prompt = std::mem::take(&mut self.input).trim().to_owned();
                    self.send(Request::GenerateSceneImage {
                        session_token: self.token.clone(),
                        conversation_id,
                        prompt,
                    });
                }
            }
            ConversationMessage::CancelSceneImage => {
                if let Some(id) = self.scene_image.request_id {
                    let _ = self.commands.send(Command::Cancel(id));
                }
            }
        }
        Task::none()
    }
}
