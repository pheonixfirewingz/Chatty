//! Request routing: the per-request context and the dispatch table.
use super::*;

/// Everything a request handler needs to answer one request on one connection.
pub(crate) struct Ctx {
    pub(crate) app: App,
    tx: mpsc::Sender<Out>,
    identity: Arc<RwLock<Option<String>>>,
    pub(crate) connection_id: String,
    pub(crate) id: u64,
    pub(crate) busy: Arc<AtomicU64>,
}

impl Ctx {
    pub(crate) fn db(&self) -> &SqlitePool {
        &self.app.db
    }

    pub(crate) fn tx(&self) -> &mpsc::Sender<Out> {
        &self.tx
    }

    pub(crate) async fn send<T: serde::Serialize>(&self, ty: MessageType, value: &T) -> Result<()> {
        self.tx.send((ty, self.id, encode(value)?.into())).await?;
        Ok(())
    }

    pub(crate) async fn respond(&self, response: Response) -> Result<()> {
        self.send(MessageType::Response, &response).await
    }

    pub(crate) async fn accepted(&self, entity_id: Option<String>, revision: i64) -> Result<()> {
        self.respond(Response::Accepted {
            entity_id,
            revision,
        })
        .await
    }

    /// Sends a persisted delta to this request and publishes it to the
    /// owner's other connections. Call only after the transaction commits.
    pub(crate) async fn deliver(&self, owner_id: &str, delta: &StateDelta) -> Result<()> {
        let encoded: Bytes = encode(delta)?.into();
        self.tx
            .send((MessageType::Delta, self.id, encoded.clone()))
            .await?;
        self.app.publish(owner_id, &self.connection_id, encoded);
        Ok(())
    }

    pub(crate) async fn delta(
        &self,
        owner_id: &str,
        revision: i64,
        entity_type: &str,
        entity_id: &str,
        operation: DeltaOperation,
        changed_fields: Vec<u8>,
    ) -> Result<()> {
        self.deliver(
            owner_id,
            &StateDelta {
                revision,
                entity_type: entity_type.into(),
                entity_id: entity_id.into(),
                operation,
                changed_fields,
            },
        )
        .await
    }

    /// Binds the connection to a user so it receives that user's live deltas.
    pub(crate) async fn set_identity(&self, user_id: Option<String>) {
        *self.identity.write().await = user_id;
    }

    /// Keeps the connection open while long, possibly silent work runs.
    pub(crate) fn busy_guard(&self) -> BusyGuard {
        BusyGuard::new(self.busy.clone())
    }

    /// Image jobs can outlast the idle limit and are cancellable by request ID
    /// or disconnect. Dropping the work cancels its pending upstream job.
    pub(crate) async fn image_job<T>(&self, work: impl Future<Output = Result<T>>) -> Result<T> {
        let _busy = self.busy_guard();
        let (cancel, mut cancel_rx) = watch::channel(false);
        let key = (self.connection_id.clone(), self.id);
        self.app
            .cancellations
            .lock()
            .await
            .insert(key.clone(), cancel);
        let result = tokio::select! {
            biased;
            _ = cancel_rx.changed() => Err(format_err!("image generation cancelled")),
            _ = self.tx.closed() => Err(format_err!("image generation cancelled")),
            result = work => result,
        };
        self.app.cancellations.lock().await.remove(&key);
        result
    }
}

pub(crate) struct DispatchControl {
    pub(crate) busy: Arc<AtomicU64>,
    pub(crate) download_cancel: Option<watch::Receiver<bool>>,
    /// Client address; keys the sign-in throttle.
    pub(crate) peer: Option<std::net::IpAddr>,
}

impl From<Arc<AtomicU64>> for DispatchControl {
    fn from(busy: Arc<AtomicU64>) -> Self {
        Self {
            busy,
            download_cancel: None,
            peer: None,
        }
    }
}

fn is_model_pull(request: &Request) -> bool {
    matches!(
        request,
        Request::AdminOllamaAction {
            action: OllamaAction::Pull { .. },
            ..
        }
    )
}

/// Register before spawning dispatch so a fast cancel or disconnect cannot race
/// authentication or initial upstream connection establishment.
pub(crate) async fn dispatch_control(
    app: &App,
    connection_id: &str,
    id: u64,
    request: &Request,
    busy: Arc<AtomicU64>,
) -> DispatchControl {
    let download_cancel = if is_model_pull(request) {
        let (sender, receiver) = watch::channel(false);
        app.cancellations
            .lock()
            .await
            .insert((connection_id.to_owned(), id), sender);
        Some(receiver)
    } else {
        None
    };
    DispatchControl {
        busy,
        download_cancel,
        peer: None,
    }
}

pub(crate) async fn dispatch(
    app: App,
    tx: mpsc::Sender<Out>,
    identity: Arc<RwLock<Option<String>>>,
    connection_id: String,
    id: u64,
    req: Request,
    control: DispatchControl,
) -> Result<()> {
    let DispatchControl {
        busy,
        download_cancel,
        peer,
    } = control;
    // Snapshots exclude mutations so the snapshot revision matches its rows.
    // Model pulls are long and touch no snapshot state, so they skip the gate.
    let _snapshot_guard = if matches!(&req, Request::Snapshot { .. }) {
        Some(app.snapshot_gate.clone().write_owned().await)
    } else {
        None
    };
    let _mutation_guard = if is_mutating(&req) && !is_model_pull(&req) {
        Some(app.snapshot_gate.clone().read_owned().await)
    } else {
        None
    };
    let ctx = Ctx {
        app,
        tx,
        identity,
        connection_id,
        id,
        busy,
    };
    let ctx = &ctx;
    use crate::{
        account::handlers as account, admin::handlers as admin, characters::handlers as characters,
        conversations::handlers as conversations, images::handlers as images,
        inference::handlers as inference, memory::handlers as memory, sync::handlers as sync,
        worlds::handlers as worlds,
    };
    use Request as R;
    match req {
        // Accounts and sessions
        R::Register { username, password } => account::register(ctx, username, password).await,
        R::Login { username, password } => account::login(ctx, peer, username, password).await,
        R::Logout { session_token } => account::logout(ctx, &session_token).await,
        R::GetServerCapabilities => account::server_capabilities(ctx).await,
        R::GetPermissions { session_token } => account::permissions(ctx, &session_token).await,
        R::GetAccountUsage { session_token } => account::usage(ctx, &session_token).await,
        // Administration
        R::AdminListUsers { session_token } => admin::list_users(ctx, &session_token).await,
        R::AdminCreateUser {
            session_token,
            username,
            password,
            role,
        } => admin::create_user(ctx, &session_token, username, password, role).await,
        R::AdminDeleteUser {
            session_token,
            user_id,
        } => admin::delete_user(ctx, &session_token, user_id).await,
        R::AdminSetRole {
            session_token,
            user_id,
            role,
        } => admin::set_role(ctx, &session_token, user_id, role).await,
        R::AdminGetBrokerConfig { session_token } => admin::get_config(ctx, &session_token).await,
        R::AdminSetBrokerConfig {
            session_token,
            config,
        } => admin::set_config(ctx, &session_token, *config).await,
        R::AdminGetBrokerMonitor { session_token } => admin::monitor(ctx, &session_token).await,
        R::AdminReadBrokerLog { session_token } => admin::read_log(ctx, &session_token).await,
        R::AdminSoftReboot { session_token } => admin::soft_reboot(ctx, &session_token).await,
        R::AdminInstallEmotionModel { session_token } => {
            admin::install_emotion_model(ctx, &session_token).await
        }
        R::AdminSearchModels {
            session_token,
            query,
        } => admin::search_models(ctx, &session_token, &query).await,
        R::AdminGetOllamaState { session_token } => admin::ollama_state(ctx, &session_token).await,
        R::AdminOllamaAction {
            session_token,
            action,
        } => admin::ollama_action(ctx, &session_token, action, download_cancel).await,
        R::AdminSetCharacterPublic {
            session_token,
            character_id,
            is_public,
        } => admin::set_character_public(ctx, &session_token, character_id, is_public).await,
        R::AdminReadDatabase { session_token } => admin::read_database(ctx, &session_token).await,
        R::AdminListImageModels { session_token } => admin::image_models(ctx, &session_token).await,
        // Characters
        R::ListCharacters { session_token } => characters::list(ctx, &session_token).await,
        R::GetCharacterImage {
            session_token,
            character_id,
            image_id,
        } => characters::image(ctx, &session_token, character_id, image_id).await,
        R::UpsertCharacter {
            session_token,
            character,
            world_ids,
        } => characters::upsert(ctx, &session_token, *character, world_ids).await,
        R::ImportCharacterFromText {
            session_token,
            source_name,
            text,
        } => characters::import_from_text(ctx, &session_token, &source_name, &text).await,
        // Worlds
        R::ListWorlds { session_token } => worlds::list(ctx, &session_token).await,
        R::SaveWorld {
            session_token,
            world,
        } => worlds::save(ctx, &session_token, world).await,
        R::DeleteWorld {
            session_token,
            world_id,
        } => worlds::delete(ctx, &session_token, world_id).await,
        R::ImportSillyTavernWorld {
            session_token,
            source_name,
            lorebook_json,
        } => worlds::import_silly_tavern(ctx, &session_token, &source_name, &lorebook_json).await,
        // Conversations and messages
        R::ListConversations { session_token } => conversations::list(ctx, &session_token).await,
        R::GetConversation {
            session_token,
            conversation_id,
        } => conversations::get(ctx, &session_token, conversation_id).await,
        R::CreateConversation {
            session_token,
            title,
            kind,
            participant_ids,
        } => conversations::create(ctx, &session_token, title, kind, participant_ids).await,
        R::UpdateConversation {
            session_token,
            conversation_id,
            title,
            kind,
            participant_ids,
        } => {
            conversations::update(
                ctx,
                &session_token,
                conversation_id,
                title,
                kind,
                participant_ids,
            )
            .await
        }
        R::UpdateConversationState {
            session_token,
            conversation_id,
            state,
            summary,
        } => {
            conversations::update_state(ctx, &session_token, conversation_id, state, summary).await
        }
        R::DeleteEntity {
            session_token,
            kind,
            entity_id,
        } => conversations::delete_entity(ctx, &session_token, kind, entity_id).await,
        R::SendMessage {
            session_token,
            conversation_id,
            content,
            speaker_id,
        } => {
            conversations::send_message(ctx, &session_token, conversation_id, content, speaker_id)
                .await
        }
        R::UpdateMessage {
            session_token,
            message_id,
            content,
        } => conversations::update_message(ctx, &session_token, message_id, content).await,
        R::SendSystemMessage {
            session_token,
            conversation_id,
            content,
        } => {
            conversations::send_system_message(ctx, &session_token, conversation_id, content).await
        }
        R::SelectVariant {
            session_token,
            message_id,
            variant_id,
        } => conversations::select_variant(ctx, &session_token, message_id, variant_id).await,
        R::Generate {
            session_token,
            conversation_id,
            speaker_id,
            parent_id,
        } => inference::generate(ctx, &session_token, conversation_id, speaker_id, parent_id).await,
        // Synchronization
        R::Snapshot { session_token } => sync::snapshot(ctx, &session_token).await,
        R::Sync {
            session_token,
            since_revision,
        } => sync::replay(ctx, session_token, since_revision, false).await,
        R::Resume {
            session_token,
            since_revision,
        } => sync::replay(ctx, session_token, since_revision, true).await,
        // Memories
        R::ListMemories {
            session_token,
            conversation_id,
            character_id,
            include_history,
        } => {
            memory::list(
                ctx,
                &session_token,
                conversation_id,
                character_id,
                include_history,
            )
            .await
        }
        R::UpsertMemory {
            session_token,
            memory,
        } => memory::upsert(ctx, &session_token, memory).await,
        R::ExtractMemory {
            session_token,
            conversation_id,
            character_id,
        } => memory::extract(ctx, &session_token, conversation_id, character_id).await,
        R::CondenseMemories {
            session_token,
            conversation_id,
            character_id,
            kind,
        } => memory::condense(ctx, &session_token, conversation_id, character_id, kind).await,
        // Images
        R::GenerateImage {
            session_token,
            prompt,
            negative_prompt,
            square,
            seed,
        } => images::generate(ctx, &session_token, prompt, negative_prompt, square, seed).await,
        R::GenerateSceneImage {
            session_token,
            conversation_id,
            prompt,
        } => images::scene(ctx, &session_token, conversation_id, prompt).await,
        R::GetMessageImage {
            session_token,
            message_id,
        } => images::message_image(ctx, &session_token, message_id).await,
    }
}

pub(crate) fn classify_error(error: &Error) -> WireError {
    let message = error.to_string();
    let lower = message.to_lowercase();
    let (code, retryable) = if lower.contains("unauthorized") || lower.contains("credentials") {
        (ErrorCode::Unauthorized, false)
    } else if lower.contains("forbidden") || lower.contains("owner") {
        (ErrorCode::Forbidden, false)
    } else if lower.contains("not found") {
        (ErrorCode::NotFound, false)
    } else if lower.contains("no model") {
        (ErrorCode::ModelMissing, true)
    } else if lower.contains("request for url")
        || lower.contains("http")
        || lower.contains("backend")
    {
        (ErrorCode::BackendUnavailable, true)
    } else if lower.contains("invalid") || lower.contains("requires") || lower.contains("must") {
        (ErrorCode::InvalidRequest, false)
    } else if lower.contains("unique constraint") {
        (ErrorCode::Conflict, false)
    } else {
        (ErrorCode::Internal, false)
    };
    WireError {
        code,
        message,
        retryable,
    }
}

pub(crate) fn is_mutating(request: &Request) -> bool {
    matches!(
        request,
        Request::Register { .. }
            | Request::Logout { .. }
            | Request::AdminSetRole { .. }
            | Request::AdminCreateUser { .. }
            | Request::AdminDeleteUser { .. }
            | Request::AdminSetBrokerConfig { .. }
            | Request::AdminOllamaAction { .. }
            | Request::AdminSetCharacterPublic { .. }
            | Request::UpsertCharacter { .. }
            | Request::CreateConversation { .. }
            | Request::UpdateConversation { .. }
            | Request::UpdateConversationState { .. }
            | Request::DeleteEntity { .. }
            | Request::SendMessage { .. }
            | Request::UpdateMessage { .. }
            | Request::SendSystemMessage { .. }
            | Request::Generate { .. }
            | Request::SelectVariant { .. }
            | Request::SaveWorld { .. }
            | Request::DeleteWorld { .. }
            | Request::UpsertMemory { .. }
            | Request::ExtractMemory { .. }
            | Request::CondenseMemories { .. }
    )
}
