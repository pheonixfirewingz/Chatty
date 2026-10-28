//! Network events, broker frames, and state deltas.

use crate::app::{ChattyApp, ImageJob, MAX_LOADED_MESSAGE_IMAGES};
use crate::characters::DraftCharacter;
use crate::memories::MemoryDialogState;
use crate::network::{Command, Event};
use crate::worlds::WorldDialogState;
use crate::{admin, characters, conversation};
use chatty_protocol::{
    BrokerLog, ChatMessage, ConversationKind, DeltaOperation, DeltaPayload, ErrorCode, Frame,
    GenerationContext, MessageType, Request, Response, Role, StateDelta, StreamChunk, Variant,
    WireError, decode,
};

impl ChattyApp {
    pub(super) fn set_network_status(&mut self, status: String) {
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

    pub(super) fn refresh(&self) {
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

    pub(super) fn authenticated(
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
        if self.user_id != user_id {
            characters::clear_image_handles();
            self.context_usage.clear();
        }
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

    pub(super) fn handle_event(&mut self, event: Event) {
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
            Event::ImageRequestStarted { request_id, scene } => {
                let job = if scene {
                    &mut self.scene_image
                } else {
                    &mut self.portrait_image
                };
                if job.pending && job.request_id.is_none() {
                    job.request_id = Some(request_id);
                } else if !scene
                    && self.admin.image_test.pending
                    && self.admin.image_test.request_id.is_none()
                {
                    self.admin.image_test.request_id = Some(request_id);
                }
            }
            Event::Status(status) => {
                if status.contains("Offline:") || status.contains("Idle · offline:") {
                    // The broker cancels image jobs when their connection closes.
                    self.scene_image = ImageJob::default();
                    self.portrait_image = ImageJob::default();
                    self.emotion_batch = None;
                    self.admin.image_test.pending = false;
                    self.message_image_requests.clear();
                    self.memory_state.condensing = false;
                    self.interrupt_models(
                        "Connection interrupted. Refresh to check whether the operation finished on the server.",
                    );
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
                self.interrupt_models("Connection failed. Try again after reconnecting.");
                self.pending_certificate = None;
                self.connected = false;
                self.connecting = false;
                self.restoring_session = false;
                self.end_generation();
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
                self.interrupt_models("Disconnected. Refresh models after reconnecting.");
                self.pending_certificate = None;
                self.connected = false;
                self.connecting = false;
                self.restoring_session = false;
                self.clear_account();
                self.status = "Enter the server IP to begin.".into();
                self.status_tooltip = None;
            }
            Event::SessionExpired => {
                self.cancel_model_downloads();
                self.restoring_session = false;
                self.clear_account();
                self.set_error("Saved session expired. Sign in again.");
            }
            Event::Frame(frame) => self.handle_frame(frame),
        }
    }

    /// Drop every value scoped to the signed-in account. The connection stays as is.
    pub(super) fn clear_account(&mut self) {
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
        self.end_generation();
        self.token.clear();
        self.role = None;
        self.context_usage.clear();
        self.character_image_requests.clear();
        self.clear_image_jobs();
    }

    /// Cancel running model downloads on the broker and forget all model state.
    pub(super) fn cancel_model_downloads(&mut self) {
        for download in self.admin.models.downloads.values() {
            if let Some(id) = download.request_id.filter(|_| !download.finished) {
                let _ = self.commands.send(Command::Cancel(id));
            }
        }
        self.admin.models = admin::ModelsState::default();
    }

    /// The link dropped: model operations end without a broker reply.
    fn interrupt_models(&mut self, notice: &str) {
        let models = &mut self.admin.models;
        models.interrupt_downloads();
        models.busy = false;
        models.request_id = None;
        models.notice = Some(notice.into());
    }

    pub(super) fn handle_frame(&mut self, frame: Frame) {
        if matches!(
            frame.message_type,
            MessageType::Response | MessageType::Error
        ) {
            if self.scene_image.owns(frame.request_id) {
                self.scene_image = ImageJob::default();
            }
            let test = &mut self.admin.image_test;
            if test.pending && test.request_id == Some(frame.request_id) {
                test.pending = false;
                test.elapsed = test.started.map(|started| started.elapsed());
                match frame.message_type {
                    MessageType::Response => {
                        if let Ok(Response::GeneratedImage { data }) =
                            decode::<Response>(&frame.payload)
                        {
                            test.result = Some(data);
                        }
                        return;
                    }
                    _ => {
                        test.notice = decode::<WireError>(&frame.payload)
                            .ok()
                            .map(|error| error.message);
                    }
                }
            }
            if self.portrait_image.owns(frame.request_id) {
                self.portrait_image = ImageJob::default();
                if frame.message_type == MessageType::Response
                    && let Ok(Response::GeneratedImage { data }) =
                        decode::<Response>(&frame.payload)
                {
                    self.add_generated_portrait(&data);
                    return;
                }
                // A failed image ends the batch; the error itself is shown below.
                self.emotion_batch = None;
            }
        }
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
                            image_generation,
                        } => {
                            self.registration_enabled = registration_enabled;
                            self.image_generation = image_generation;
                        }
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
                            let mut draft = DraftCharacter::from(character.as_ref());
                            draft.owned_by_user = true;
                            self.set_draft(draft);
                            self.char_import_pending = false;
                            self.char_import_started = None;
                            self.draft_character_open = true;
                        }
                        Response::Characters(v) => self.set_characters(v),
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
                            self.store_character_image(&character_id, &image_id, data);
                        }
                        Response::Conversations(v) => {
                            self.conversations = v
                                .into_iter()
                                .filter(|conversation| {
                                    conversation.kind == ConversationKind::Direct
                                })
                                .collect();
                            self.context_usage.retain(|id, _| {
                                self.conversations
                                    .iter()
                                    .any(|conversation| &conversation.id == id)
                            });
                            if self.selected_conversation.as_ref().is_some_and(|selected| {
                                !self
                                    .conversations
                                    .iter()
                                    .any(|conversation| &conversation.id == selected)
                            }) {
                                self.close_conversation();
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
                            self.request_message_images();
                            if self.tts_auto_speak
                                && let Some(message_id) = self.pending_tts_message_id.take()
                                && let Some(message) = self
                                    .messages
                                    .iter()
                                    .find(|message| message.id == message_id)
                            {
                                let text =
                                    conversation::displayed_message_content(message).to_owned();
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
                                self.close_conversation();
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
                            image_plugins,
                        } => {
                            self.image_generation = v.image.enabled;
                            self.admin.image_plugins = image_plugins;
                            self.admin.adapter_plugins = adapter_plugins;
                            self.admin.adapter_override = adapter_override;
                            self.admin.models.saved_adapter_url = Some(v.adapter_url.clone());
                            self.admin.models.saved_model = v.model.clone();
                            self.admin.set_broker_config(v);
                        }
                        Response::BrokerMonitor(v) => self.admin.broker_monitor = Some(v),
                        Response::BrokerLog(v) => self.admin.admin_log = v,
                        Response::ModelSearchResults(v) => {
                            self.admin.models.results = v;
                            self.admin.models.selected_files.clear();
                        }
                        Response::OllamaState(v) => self.admin.ollama_state = Some(v),
                        Response::ImageModels(v) => self.admin.image_models = Some(v),
                        Response::MessageImage { message_id, data } => {
                            if self.message_image_requests.remove(&message_id) {
                                self.message_images.insert(message_id, data);
                            }
                        }
                        Response::AdminDatabase(v) => self.admin.admin_data = v,
                        Response::GenerationStarted {
                            character_id,
                            context,
                            ..
                        } => {
                            self.update_context_usage(context);
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
                            context,
                        } => {
                            self.update_context_usage(context);
                            self.pending_tts_message_id = (!cancelled).then_some(message_id);
                            self.finish_generation(revision);
                        }
                        Response::Accepted { revision, .. } => {
                            self.revision = self.revision.max(revision);
                            let done = match self.memory_state.status_message.as_deref() {
                                Some("Saving memory…") => Some("Memory saved."),
                                Some("Removing memory…") => Some("Memory removed."),
                                Some("Learning from chat…") => {
                                    Some("New memory learned from the chat.")
                                }
                                _ => None,
                            };
                            if let Some(done) = done {
                                self.memory_state.status_message = Some(done.into());
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
                    context,
                }) = decode::<Response>(&frame.payload)
                {
                    self.update_context_usage(context);
                    self.pending_tts_message_id = (!cancelled).then_some(message_id);
                    self.finish_generation(revision);
                } else {
                    self.end_generation();
                }
            }
            MessageType::Delta => {
                if let Ok(d) = decode::<StateDelta>(&frame.payload) {
                    let message = d.entity_type == "message";
                    self.apply_delta(d);
                    if message {
                        self.request_message_images();
                    }
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
                    self.end_generation();
                }
            }
            _ => {}
        }
    }

    pub(super) fn clear_image_jobs(&mut self) {
        self.scene_image = ImageJob::default();
        self.portrait_image = ImageJob::default();
        self.emotion_batch = None;
        self.message_images.clear();
        self.message_image_requests.clear();
    }

    /// Keep bytes only for the newest image messages in the open chat.
    pub(super) fn request_message_images(&mut self) {
        let wanted: Vec<String> = self
            .messages
            .iter()
            .rev()
            .filter(|message| message.author_type == "image")
            .take(MAX_LOADED_MESSAGE_IMAGES)
            .map(|message| message.id.clone())
            .collect();
        self.message_images.retain(|id, _| wanted.contains(id));
        self.message_image_requests.retain(|id| wanted.contains(id));
        for id in wanted {
            if !self.message_images.contains_key(&id)
                && self.message_image_requests.insert(id.clone())
            {
                self.send(Request::GetMessageImage {
                    session_token: self.token.clone(),
                    message_id: id,
                });
            }
        }
    }

    pub(super) fn update_context_usage(&mut self, context: GenerationContext) {
        if self
            .conversations
            .iter()
            .any(|conversation| conversation.id == context.conversation_id)
        {
            self.context_usage
                .insert(context.conversation_id.clone(), context);
        }
    }

    pub(super) fn apply_delta(&mut self, d: StateDelta) {
        let payload = if matches!(d.operation, DeltaOperation::Delete) {
            None
        } else {
            decode::<DeltaPayload>(&d.changed_fields).ok()
        };
        if d.entity_type == "conversation" && matches!(d.operation, DeltaOperation::Delete) {
            self.context_usage.remove(&d.entity_id);
        }
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
                && self.selected_conversation.as_deref() == Some(conversation_id.as_str())
            {
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
        } else if d.entity_type == "variant"
            && let Some(DeltaPayload::Variant {
                message_id,
                content,
                character_image_id,
            }) = payload.as_ref()
            && let Some(message) = self
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
        self.revision = self.revision.max(d.revision);
        let key = (d.entity_type, d.entity_id);
        if matches!(d.operation, DeltaOperation::Delete) {
            self.state.remove(&key);
        } else if let Some(p) = payload {
            self.state.insert(key, p);
        }
    }
}
