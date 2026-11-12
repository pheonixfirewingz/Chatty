//! World requests.
use super::*;

pub(crate) async fn list(ctx: &Ctx, session_token: &str) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    ctx.respond(Response::Worlds(load_worlds(ctx.db(), &uid).await?))
        .await
}

pub(crate) async fn save(ctx: &Ctx, session_token: &str, mut world: World) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    if let Err(error) = world.validate() {
        bail!("{error}")
    }
    if world.id.is_empty() {
        world.id = new_uuid();
    }
    let data = encode(&world)?;
    let mut transaction = ctx.db().begin().await?;
    let owner: Option<String> = sqlx::query_scalar("SELECT owner_id FROM worlds WHERE id=?")
        .bind(&world.id)
        .fetch_optional(&mut *transaction)
        .await?;
    if owner.as_ref().is_some_and(|owner| owner != &uid) {
        bail!("forbidden world owner")
    }
    for character_id in &world.character_ids {
        if !character_accessible(&mut *transaction, &uid, character_id).await? {
            bail!("character unavailable")
        }
    }
    let used: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(length(data)),0) FROM worlds WHERE owner_id=? AND id<>?",
    )
    .bind(&uid)
    .bind(&world.id)
    .fetch_one(&mut *transaction)
    .await?;
    if used as usize + data.len() > MAX_WORLD_LIBRARY_BYTES {
        bail!("World library exceeds 4 MiB")
    }
    let operation = if owner.is_some() {
        DeltaOperation::Update
    } else {
        DeltaOperation::Add
    };
    let world_id = world.id.clone();
    let changed = encode(&DeltaPayload::World(world))?;
    let rev = delta_tx(
        &mut transaction,
        &uid,
        "world",
        &world_id,
        operation,
        &changed,
    )
    .await?;
    sqlx::query("INSERT INTO worlds(id,owner_id,data,revision) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data,revision=excluded.revision WHERE owner_id=excluded.owner_id")
        .bind(&world_id)
        .bind(&uid)
        .bind(data)
        .bind(rev)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    ctx.delta(&uid, rev, "world", &world_id, operation, changed)
        .await?;
    ctx.accepted(Some(world_id), rev).await
}

pub(crate) async fn delete(ctx: &Ctx, session_token: &str, world_id: String) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let mut transaction = ctx.db().begin().await?;
    let deleted = sqlx::query("DELETE FROM worlds WHERE id=? AND owner_id=?")
        .bind(&world_id)
        .bind(&uid)
        .execute(&mut *transaction)
        .await?
        .rows_affected();
    if deleted == 0 {
        bail!("world not found")
    }
    let changed = encode(&DeltaPayload::Empty)?;
    let rev = delta_tx(
        &mut transaction,
        &uid,
        "world",
        &world_id,
        DeltaOperation::Delete,
        &changed,
    )
    .await?;
    transaction.commit().await?;
    ctx.delta(
        &uid,
        rev,
        "world",
        &world_id,
        DeltaOperation::Delete,
        changed,
    )
    .await?;
    ctx.accepted(Some(world_id), rev).await
}

pub(crate) async fn import_silly_tavern(
    ctx: &Ctx,
    session_token: &str,
    source_name: &str,
    lorebook_json: &str,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let _busy = ctx.busy_guard();
    let world = import_silly_tavern_world(&ctx.app, &uid, source_name, lorebook_json).await?;
    ctx.respond(Response::WorldImportPreview(world)).await
}
