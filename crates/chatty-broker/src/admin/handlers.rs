//! Administrator requests. Every handler checks the admin role first.
use super::*;

pub(crate) async fn list_users(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    let users = account::list_users(ctx.db()).await?;
    ctx.respond(Response::Users(users)).await
}

pub(crate) async fn create_user(
    ctx: &Ctx,
    session_token: &str,
    username: String,
    password: String,
    role: Role,
) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    validate_credentials(&username, &password)?;
    let uid = new_uuid();
    let hash = account::hash_password(&ctx.app, &password).await?;
    sqlx::query("INSERT INTO users(id,username,password_hash,role) VALUES(?,?,?,?)")
        .bind(&uid)
        .bind(username.trim())
        .bind(hash)
        .bind(role_to_db(role))
        .execute(ctx.db())
        .await?;
    ctx.accepted(Some(uid), 0).await
}

pub(crate) async fn delete_user(ctx: &Ctx, session_token: &str, user_id: String) -> Result<()> {
    let admin_id = require_admin(ctx.db(), session_token).await?;
    if admin_id == user_id {
        bail!("administrators cannot delete their own active account")
    }
    let mut transaction = ctx.db().begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id=?)")
        .bind(&user_id)
        .fetch_one(&mut *transaction)
        .await?;
    if !exists {
        bail!("user not found")
    }
    sqlx::query("DELETE FROM memories WHERE owner_id=? OR character_id IN (SELECT id FROM characters WHERE owner_id=?)")
        .bind(&user_id)
        .bind(&user_id)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("DELETE FROM participants WHERE character_id IN (SELECT id FROM characters WHERE owner_id=?)")
        .bind(&user_id)
        .execute(&mut *transaction)
        .await?;
    for statement in [
        "DELETE FROM conversations WHERE owner_id=?",
        "DELETE FROM characters WHERE owner_id=?",
        "DELETE FROM deltas WHERE owner_id=?",
        "DELETE FROM sessions WHERE user_id=?",
        "DELETE FROM users WHERE id=?",
    ] {
        sqlx::query(statement)
            .bind(&user_id)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    ctx.accepted(Some(user_id), 0).await
}

pub(crate) async fn set_role(
    ctx: &Ctx,
    session_token: &str,
    user_id: String,
    role: Role,
) -> Result<()> {
    let admin_id = require_admin(ctx.db(), session_token).await?;
    if admin_id == user_id && role == Role::User {
        bail!("administrators cannot demote their own active account")
    }
    let changed = sqlx::query("UPDATE users SET role=? WHERE id=?")
        .bind(role_to_db(role))
        .bind(&user_id)
        .execute(ctx.db())
        .await?
        .rows_affected();
    if changed != 1 {
        bail!("user not found")
    }
    ctx.accepted(Some(user_id), 0).await
}

pub(crate) async fn get_config(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    respond_config(ctx).await
}

pub(crate) async fn set_config(ctx: &Ctx, session_token: &str, config: BrokerConfig) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    let config = save_broker_config(&ctx.app, config).await?;
    ctx.respond(ctx.app.adapters.config_response(config)).await
}

pub(crate) async fn monitor(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    ctx.respond(Response::BrokerMonitor(broker_monitor(&ctx.app).await))
        .await
}

pub(crate) async fn read_log(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    ctx.respond(Response::BrokerLog(read_broker_log(&ctx.app.log_path)?))
        .await
}

pub(crate) async fn soft_reboot(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    ctx.accepted(None, 0).await?;
    // Give the writer time to flush the acknowledgement; the service manager
    // restarts the broker.
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(150)).await;
        std::process::exit(0);
    });
    Ok(())
}

pub(crate) async fn install_emotion_model(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    // The download can outlast the idle limit between progress frames.
    let _busy = ctx.busy_guard();
    let path = tokio::select! {
        biased;
        _ = ctx.tx().closed() => return Ok(()),
        path = install_recommended_model(ctx.tx(), ctx.id) => path?,
    };
    let path = path
        .to_str()
        .context("emotion classifier path is not valid UTF-8")?
        .to_owned();
    sqlx::query(
        "UPDATE broker_settings SET emotion_model=?,updated_at=CURRENT_TIMESTAMP WHERE singleton=1",
    )
    .bind(&path)
    .execute(ctx.db())
    .await?;
    info!(path = %path, "installed recommended emotion classifier");
    respond_config(ctx).await
}

async fn respond_config(ctx: &Ctx) -> Result<()> {
    let config = load_broker_config(ctx.db()).await?;
    ctx.respond(ctx.app.adapters.config_response(config)).await
}

pub(crate) async fn search_models(ctx: &Ctx, session_token: &str, query: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    let results = search_hugging_face_models(&ctx.app, query).await?;
    ctx.respond(Response::ModelSearchResults(results)).await
}

pub(crate) async fn ollama_state(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    ctx.respond(Response::OllamaState(load_ollama_state(&ctx.app).await?))
        .await
}

pub(crate) async fn ollama_action(
    ctx: &Ctx,
    session_token: &str,
    action: OllamaAction,
    download_cancel: Option<watch::Receiver<bool>>,
) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    // Model pulls can stay silent longer than the connection idle limit.
    let _busy = ctx.busy_guard();
    if let OllamaAction::Pull { model } = action {
        let cancel = download_cancel.context("download cancellation registration missing")?;
        download_model(&ctx.app, ctx.tx(), ctx.id, model, cancel).await
    } else {
        run_ollama_action(&ctx.app, action).await?;
        ctx.respond(Response::OllamaState(load_ollama_state(&ctx.app).await?))
            .await
    }
}

pub(crate) async fn set_character_public(
    ctx: &Ctx,
    session_token: &str,
    character_id: String,
    is_public: bool,
) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    let changed = sqlx::query("UPDATE characters SET is_public=? WHERE id=?")
        .bind(is_public)
        .bind(&character_id)
        .execute(ctx.db())
        .await?
        .rows_affected();
    if changed != 1 {
        bail!("character not found")
    }
    ctx.accepted(Some(character_id), 0).await
}

pub(crate) async fn read_database(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    let mut data = Vec::new();
    for row in
        sqlx::query("SELECT id,username,role,created_at FROM users ORDER BY created_at LIMIT 1000")
            .fetch_all(ctx.db())
            .await?
    {
        data.push(AdminDataRow {
            kind: "User".into(),
            id: row.get("id"),
            label: row.get("username"),
            detail: format!(
                "{} · {}",
                row.get::<&str, _>("role"),
                row.get::<&str, _>("created_at")
            ),
            is_public: None,
        });
    }
    for row in sqlx::query("SELECT c.id,c.name,c.is_public,u.username FROM characters c JOIN users u ON u.id=c.owner_id ORDER BY c.name LIMIT 1000")
        .fetch_all(ctx.db())
        .await?
    {
        data.push(AdminDataRow {
            kind: "Character".into(),
            id: row.get("id"),
            label: row.get("name"),
            detail: format!("Owner: {}", row.get::<&str, _>("username")),
            is_public: Some(row.get("is_public")),
        });
    }
    let config = load_broker_config(ctx.db()).await?;
    data.push(AdminDataRow {
        kind: "Setting".into(),
        id: "adapter".into(),
        label: "Adapter".into(),
        detail: format!(
            "{} · {}",
            if config.adapter_enabled {
                "enabled"
            } else {
                "disabled"
            },
            config.adapter_url
        ),
        is_public: None,
    });
    ctx.respond(Response::AdminDatabase(data)).await
}

pub(crate) async fn image_models(ctx: &Ctx, session_token: &str) -> Result<()> {
    require_admin(ctx.db(), session_token).await?;
    ctx.respond(Response::ImageModels(
        crate::images::image_models(&ctx.app).await?,
    ))
    .await
}
