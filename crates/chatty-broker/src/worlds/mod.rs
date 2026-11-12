//! Worlds: shared lore that groups characters.
use super::*;

pub(crate) mod handlers;

/// Largest total encoded size of one owner's worlds.
const MAX_WORLD_LIBRARY_BYTES: usize = 4 * 1024 * 1024;

pub(crate) async fn load_worlds(db: &SqlitePool, uid: &str) -> Result<Vec<World>> {
    let rows: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT data FROM worlds WHERE owner_id=? ORDER BY id")
            .bind(uid)
            .fetch_all(db)
            .await?;
    rows.iter()
        .map(|data| decode(data).map_err(Into::into))
        .collect()
}

/// Links `character_id` to exactly the owner's worlds in `world_ids`
/// (sorted, deduplicated) and unlinks it from the rest. Appends one delta per
/// changed world and returns them for delivery after commit.
pub(crate) async fn link_character(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    owner_id: &str,
    character_id: &str,
    world_ids: &[String],
) -> Result<Vec<StateDelta>> {
    let rows = sqlx::query("SELECT id,data FROM worlds WHERE owner_id=? ORDER BY id")
        .bind(owner_id)
        .fetch_all(&mut **transaction)
        .await?;
    if world_ids
        .iter()
        .any(|requested| !rows.iter().any(|row| row.get::<&str, _>("id") == requested))
    {
        bail!("linked world unavailable")
    }
    let mut deltas = Vec::new();
    for row in rows {
        let mut world: World = decode(row.get::<&[u8], _>("data"))?;
        let should_link = world_ids.binary_search(&world.id).is_ok();
        let is_linked = world.character_ids.iter().any(|id| id == character_id);
        if should_link == is_linked {
            continue;
        }
        world.character_ids.retain(|id| id != character_id);
        if should_link {
            world.character_ids.push(character_id.to_owned());
        }
        world.validate().map_err(Error::msg)?;
        let data = encode(&world)?;
        let changed = encode(&DeltaPayload::World(world))?;
        let world_id: String = row.get("id");
        let revision = delta_tx(
            transaction,
            owner_id,
            "world",
            &world_id,
            DeltaOperation::Update,
            &changed,
        )
        .await?;
        sqlx::query("UPDATE worlds SET data=?,revision=? WHERE id=? AND owner_id=?")
            .bind(data)
            .bind(revision)
            .bind(&world_id)
            .bind(owner_id)
            .execute(&mut **transaction)
            .await?;
        deltas.push(StateDelta {
            revision,
            entity_type: "world".into(),
            entity_id: world_id,
            operation: DeltaOperation::Update,
            changed_fields: changed,
        });
    }
    Ok(deltas)
}
