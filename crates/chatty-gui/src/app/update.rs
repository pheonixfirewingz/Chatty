//! Message dispatch and the feature-level update handlers.

use crate::admin;
use crate::admin::AdminMessage;
use crate::app::{ChattyApp, Message, Screen};
use crate::characters::DraftCharacter;
use crate::conversation::{self, ConversationMessage};
use crate::memories::{MemoryAction, MemoryDialogState, MemoryMessage};
use crate::network::{Command, connection_target, save_preferences};
use crate::tts::TtsEvent;
use crate::worlds::{WorldAction, WorldMessage};
use chatty_protocol::{ChatMessage, Conversation, ConversationKind, EntityKind, Request};

/// The broker accepts at most 32 characters per conversation.
use iced::Task;
use iced::widget::text_editor;
use std::time::{Duration, Instant};

impl ChattyApp {
    pub(super) fn update_message(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowOpened(id) => iced::window::run(id, |window| {
                if !crate::capture_guard::exclude_from_capture(window)
                    && !crate::capture_guard::HIDE_UNFOCUSED
                {
                    eprintln!("chatty: could not exclude the window from screen capture");
                }
            })
            .discard(),
            Message::WindowFocusChanged(focused) => {
                self.window_focused = focused;
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
                self.follow_chat_bottom()
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
                self.open_chat_dialog(Vec::new());
                Task::none()
            }
            Message::OpenCharactersDialog => {
                self.draft_character_open = true;
                self.set_draft(DraftCharacter::default());
                self.enqueue_missing_character_images();
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
            Message::World(msg) => self.update_world(msg),
            Message::Memory(msg) => self.update_memory(msg),
            Message::Conversation(msg) => self.update_conversation(msg),
            Message::Admin(msg) => self.update_admin(msg),
            Message::Tts(event) => {
                self.update_tts(event);
                Task::none()
            }
            Message::Network(event) => {
                self.handle_event(event);
                self.network_tasks()
            }
            Message::RetryMessageImages => {
                self.retry_message_images();
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
        self.shown_conversation = None;
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
        if self.selected_conversation.as_deref() != Some(id) {
            self.next_speaker = None;
        }
        self.auto_select_conversation = true;
        self.selected_conversation = Some(id.into());
        self.send(Request::GetConversation {
            session_token: self.token.clone(),
            conversation_id: id.into(),
        });
    }

    pub(super) fn submit_message(&mut self) {
        let Some(cid) = self.selected_conversation.clone() else {
            self.open_chat_dialog(Vec::new());
            return;
        };
        let speaker = self.selected_speaker().map(str::to_owned);
        // Outside manual mode, picking a group speaker applies to one reply.
        if self
            .selected_conversation_entry()
            .is_some_and(|c| c.kind != ConversationKind::GroupManual)
        {
            self.next_speaker = None;
        }
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

    pub(super) fn selected_conversation_entry(&self) -> Option<&Conversation> {
        let id = self.selected_conversation.as_deref()?;
        self.conversations.iter().find(|c| c.id == id)
    }

    /// The character that replies next, or `None` to let the broker choose.
    ///
    /// A direct chat always uses its character. A group uses the speaker the
    /// user picked; manual mode falls back to the first character because the
    /// broker requires an explicit speaker there.
    pub(super) fn selected_speaker(&self) -> Option<&str> {
        let conversation = self.selected_conversation_entry()?;
        let participants = &conversation.participant_ids;
        if participants.len() < 2 {
            return participants.first().map(String::as_str);
        }
        self.next_speaker
            .as_deref()
            .filter(|id| participants.iter().any(|p| p == id))
            .or_else(|| {
                (conversation.kind == ConversationKind::GroupManual)
                    .then(|| participants.first().map(String::as_str))
                    .flatten()
            })
    }

    /// The character whose memories a manual extraction updates: the chosen
    /// speaker, else the latest character to reply in the open chat.
    /// Character that manual extraction is limited to: the dialog's character
    /// filter when it takes part in the selected chat. `None` extracts for
    /// every participant.
    pub(super) fn memory_extraction_character(&self) -> Option<&str> {
        let character_id = self.memory_state.filter_character_id.as_deref()?;
        let conversation_id = self.selected_conversation.as_deref()?;
        self.conversations
            .iter()
            .find(|conversation| conversation.id == conversation_id)
            .is_some_and(|conversation| {
                conversation
                    .participant_ids
                    .iter()
                    .any(|id| id == character_id)
            })
            .then_some(character_id)
    }

    /// Open the chat dialog to create a conversation with `characters`.
    pub(super) fn open_chat_dialog(&mut self, characters: Vec<String>) {
        self.selected_characters = characters;
        self.new_chat_kind = ConversationKind::GroupAutomatic;
        self.editing_conversation = None;
        self.new_conversation_title.clear();
        self.new_chat_search.clear();
        self.new_chat_open = true;
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
            include_history: true,
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

    pub(super) fn update_world(&mut self, msg: WorldMessage) -> Task<Message> {
        match msg {
            WorldMessage::ImportLorebook => {
                if self.file_dialog_open || self.world_state.import_pending {
                    return Task::none();
                }
                self.file_dialog_open = true;
                return Task::perform(
                    crate::worlds::pick_silly_tavern_lorebook_async(),
                    |outcome| Message::World(WorldMessage::LorebookPicked(outcome)),
                );
            }
            WorldMessage::LorebookPicked(outcome) => {
                self.file_dialog_open = false;
                if self.token.is_empty() {
                    return Task::none();
                }
                let action = self
                    .world_state
                    .update(&self.worlds, WorldMessage::LorebookPicked(outcome));
                self.apply_world_action(action);
            }
            other => {
                let action = self.world_state.update(&self.worlds, other);
                self.apply_world_action(action);
            }
        }
        Task::none()
    }

    fn apply_world_action(&mut self, action: WorldAction) {
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
                let char_id = self.memory_extraction_character().map(str::to_owned);
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

    pub(super) fn update_admin(&mut self, msg: AdminMessage) -> Task<Message> {
        if matches!(msg, AdminMessage::CloseDialog) {
            self.screen = Screen::Chat;
        } else if matches!(msg, AdminMessage::RefreshMonitor) {
            self.refresh_admin_monitor_if_due();
        } else if let AdminMessage::CancelDownload(model) = &msg {
            if let Some(id) = self.admin.models.cancel_download(model) {
                let _ = self.commands.send(Command::Cancel(id));
            }
        } else if matches!(msg, AdminMessage::LoadImageWorkflow) {
            if self.file_dialog_open {
                return Task::none();
            }
            self.file_dialog_open = true;
            return Task::perform(admin::pick_image_workflow_async(), |outcome| {
                Message::Admin(AdminMessage::ImageWorkflowLoaded(outcome))
            });
        } else if let AdminMessage::ImageWorkflowLoaded(outcome) = msg {
            self.file_dialog_open = false;
            if !self.token.is_empty() {
                let reqs = self
                    .admin
                    .update(AdminMessage::ImageWorkflowLoaded(outcome), &self.token);
                for req in reqs {
                    self.send(req);
                }
            }
        } else {
            let reqs = self.admin.update(msg, &self.token);
            for req in reqs {
                self.send(req);
            }
        }
        Task::none()
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

    /// Show the newest message and follow new content from now on.
    fn follow_chat_bottom(&mut self) -> Task<Message> {
        self.chat_at_bottom = true;
        conversation::scroll_chat_to_bottom()
    }

    /// Tasks that network events requested: a chat switch or an image retry.
    fn network_tasks(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if std::mem::take(&mut self.chat_follow_due) {
            tasks.push(self.follow_chat_bottom());
        }
        if std::mem::take(&mut self.message_image_retry_due) {
            self.message_image_retry_scheduled = true;
            // 2, 4, then 8 seconds as failures repeat.
            let failures = self
                .message_image_failures
                .values()
                .copied()
                .max()
                .unwrap_or(1);
            let delay = std::time::Duration::from_secs(1 << failures.min(3));
            // Create the timer when the executor polls it, inside its runtime.
            tasks.push(Task::perform(
                async move { tokio::time::sleep(delay).await },
                |()| Message::RetryMessageImages,
            ));
        }
        Task::batch(tasks)
    }

    pub(super) fn update_conversation(&mut self, msg: ConversationMessage) -> Task<Message> {
        match msg {
            ConversationMessage::InputChanged(val) => {
                self.input = val;
            }
            ConversationMessage::SubmitMessage => {
                self.submit_message();
                return self.follow_chat_bottom();
            }
            ConversationMessage::ChatScrolled {
                from_top,
                from_bottom,
            } => {
                let at_bottom = from_bottom <= conversation::CHAT_BOTTOM_TOLERANCE;
                if at_bottom != self.chat_at_bottom {
                    self.chat_at_bottom = at_bottom;
                    // The anchor flips with this flag: keep the same position
                    // measured from the new anchor. iced runs widget operations
                    // from `update` after rebuilding the view and before drawing,
                    // so no frame shows the old offset under the new anchor.
                    return conversation::scroll_chat_to(if at_bottom {
                        from_bottom
                    } else {
                        from_top
                    });
                }
            }
            ConversationMessage::CancelRequest(id) => {
                let _ = self.commands.send(Command::Cancel(id));
            }
            ConversationMessage::StartEditMessage { id, content } => {
                self.editing_message_id = Some(id);
                self.editing_message_content = text_editor::Content::with_text(&content);
                // Focus at once so typing, Ctrl+Enter, and Esc reach the editor.
                return iced::widget::operation::focus(conversation::edit_editor_id());
            }
            ConversationMessage::EditAction(action) => {
                // Only edits change the message; the text is read when saving.
                return crate::ui::perform_editor_action(
                    &mut self.editing_message_content,
                    action,
                    conversation::edit_scroll_id(),
                )
                .1;
            }
            ConversationMessage::SaveMessageEdit => {
                if let Some(id) = self.editing_message_id.take() {
                    let content = self.editing_message_content.text();
                    self.editing_message_content = text_editor::Content::new();
                    self.send(Request::UpdateMessage {
                        session_token: self.token.clone(),
                        message_id: id,
                        content,
                    });
                }
            }
            ConversationMessage::CancelMessageEdit => {
                self.editing_message_id = None;
                self.editing_message_content = text_editor::Content::new();
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
                self.open_chat_dialog(Vec::new());
            }
            ConversationMessage::EditConversation => {
                if let Some(conversation) = self.selected_conversation_entry().cloned() {
                    self.selected_characters = conversation.participant_ids;
                    self.new_chat_kind = match conversation.kind {
                        ConversationKind::Direct => ConversationKind::GroupAutomatic,
                        kind => kind,
                    };
                    self.new_conversation_title = conversation.title;
                    self.editing_conversation = Some(conversation.id);
                    self.new_chat_search.clear();
                    self.new_chat_open = true;
                }
            }
            ConversationMessage::CloseNewChatDialog => {
                self.new_chat_open = false;
                self.editing_conversation = None;
            }
            ConversationMessage::NewChatSearchChanged(query) => {
                self.new_chat_search = query;
            }
            ConversationMessage::NewChatTitleChanged(title) => {
                self.new_conversation_title = title;
            }
            ConversationMessage::ToggleNewChatCharacter(id) => {
                if let Some(index) = self.selected_characters.iter().position(|c| *c == id) {
                    self.selected_characters.remove(index);
                } else if self.selected_characters.len() < conversation::MAX_CHAT_CHARACTERS
                    && conversation::can_join_group(&self.worlds, &self.selected_characters, &id)
                {
                    self.selected_characters.push(id);
                }
            }
            ConversationMessage::NewChatModeChanged(kind) => {
                self.new_chat_kind = kind;
            }
            ConversationMessage::CreateNewChat => {
                if self.selected_characters.is_empty() {
                    return Task::none();
                }
                let participant_ids = std::mem::take(&mut self.selected_characters);
                let kind =
                    conversation::kind_for_selection(participant_ids.len(), self.new_chat_kind);
                let title = match self.new_conversation_title.trim() {
                    "" if participant_ids.len() > 1 => "Group chat".to_string(),
                    "" => "New Conversation".to_string(),
                    title => title.to_string(),
                };
                let request = match self.editing_conversation.take() {
                    Some(conversation_id) => Request::UpdateConversation {
                        session_token: self.token.clone(),
                        conversation_id,
                        title,
                        kind,
                        participant_ids,
                    },
                    None => {
                        self.pending_new_conversation = true;
                        Request::CreateConversation {
                            session_token: self.token.clone(),
                            title,
                            kind,
                            participant_ids,
                        }
                    }
                };
                self.send(request);
                self.new_chat_open = false;
                self.compact_sidebar_open = false;
                self.new_conversation_title.clear();
            }
            ConversationMessage::SelectNextSpeaker(id) => {
                self.next_speaker = id;
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
