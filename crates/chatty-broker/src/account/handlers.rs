//! Account requests: registration, sign-in, sign-out, and per-user queries.
use super::*;

/// Starts a session for `user_id`, binds the connection, and answers.
async fn sign_in(ctx: &Ctx, user_id: String, role: Role) -> Result<()> {
    let (session_token, revision) = new_session(ctx.db(), &user_id).await?;
    ctx.set_identity(Some(user_id.clone())).await;
    ctx.respond(Response::Authenticated {
        session_token,
        user_id,
        role,
        revision,
    })
    .await
}

pub(crate) async fn register(ctx: &Ctx, username: String, password: String) -> Result<()> {
    validate_credentials(&username, &password)?;
    let (user_count, registration_enabled): (i64, bool) = sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM users),allow_self_registration FROM broker_settings WHERE singleton=1",
    )
    .fetch_one(ctx.db())
    .await?;
    if user_count > 0 && !registration_enabled {
        bail!("self registration is disabled")
    }
    let uid = new_uuid();
    let hash = hash_password(&ctx.app, &password).await?;
    // The first account becomes the administrator.
    let role: String = sqlx::query_scalar("INSERT INTO users(id,username,password_hash,role) VALUES(?,?,?,CASE WHEN EXISTS(SELECT 1 FROM users) THEN 'user' ELSE 'admin' END) RETURNING role")
        .bind(&uid)
        .bind(username)
        .bind(hash)
        .fetch_one(ctx.db())
        .await?;
    sign_in(ctx, uid, role_from_db(&role)).await
}

pub(crate) async fn login(
    ctx: &Ctx,
    peer: Option<std::net::IpAddr>,
    username: String,
    password: String,
) -> Result<()> {
    let throttle_key = login_throttle_key(peer, &username);
    login_throttle().check(&throttle_key, Instant::now())?;
    let row = sqlx::query("SELECT id,password_hash,role FROM users WHERE username=?")
        .bind(&username)
        .fetch_optional(ctx.db())
        .await?;
    // Verify against a dummy hash for unknown users so response time
    // does not reveal which usernames exist.
    let verified = {
        let _argon_permit = ctx.app.argon_permit().await?;
        let stored: String = match &row {
            Some(row) => row.get("password_hash"),
            None => dummy_password_hash()?.to_owned(),
        };
        PasswordHash::new(&stored).is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    };
    if !verified {
        login_throttle().failed(&throttle_key, Instant::now());
    }
    let row = row.filter(|_| verified).context("invalid credentials")?;
    login_throttle().succeeded(&throttle_key);
    let role: String = row.get("role");
    sign_in(ctx, row.get("id"), role_from_db(&role)).await
}

pub(crate) async fn logout(ctx: &Ctx, session_token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token=?")
        .bind(session_key(session_token))
        .execute(ctx.db())
        .await?;
    ctx.set_identity(None).await;
    ctx.accepted(None, 0).await
}

pub(crate) async fn server_capabilities(ctx: &Ctx) -> Result<()> {
    let (registration_enabled, image_generation): (bool, bool) = sqlx::query_as(
        "SELECT allow_self_registration,image_enabled FROM broker_settings WHERE singleton=1",
    )
    .fetch_one(ctx.db())
    .await?;
    ctx.respond(Response::ServerCapabilities {
        registration_enabled,
        image_generation,
    })
    .await
}

pub(crate) async fn permissions(ctx: &Ctx, session_token: &str) -> Result<()> {
    let role: String = sqlx::query_scalar("SELECT u.role FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.token=? AND s.expires_at>datetime('now')")
        .bind(session_key(session_token))
        .fetch_optional(ctx.db())
        .await?
        .context("unauthorized")?;
    let mut permissions = vec![Permission::ManageOwnRoleplay, Permission::GenerateRoleplay];
    if role_from_db(&role) == Role::Admin {
        permissions.push(Permission::ManageUsers);
    }
    ctx.respond(Response::Permissions(permissions)).await
}

pub(crate) async fn usage(ctx: &Ctx, session_token: &str) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let row = sqlx::query("SELECT prompt_tokens,completion_tokens FROM users WHERE id=?")
        .bind(uid)
        .fetch_one(ctx.db())
        .await?;
    ctx.respond(Response::AccountUsage(token_usage(&row))).await
}
