//! Character requests.
use super::*;

pub(crate) async fn list(ctx: &Ctx, session_token: &str) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let mut rows = sqlx::query(
        "SELECT * FROM characters WHERE owner_id=? OR is_public=1 ORDER BY name LIMIT 500",
    )
    .bind(&uid)
    .fetch(ctx.db());
    let mut characters = Vec::new();
    while let Some(row) = rows.try_next().await? {
        let input = character_from_row(&ctx.app, &row, &uid).await?;
        characters.push(Character {
            id: input.id.unwrap_or_default(),
            name: input.name,
            description: input.description,
            personality: input.personality,
            scenario: input.scenario,
            system_prompt: input.system_prompt,
            example_dialogue: input.example_dialogue,
            appearance: input.appearance,
            age: input.age,
            gender: input.gender,
            race: input.race,
            misc: input.misc,
            tags: input.tags,
            avatar: input.avatar,
            default_image_id: input.default_image_id,
            images: input.images,
            is_public: input.is_public,
            owned_by_user: input.owned_by_user,
            revision: row.get("revision"),
        });
    }
    ctx.respond(Response::Characters(characters)).await
}

pub(crate) async fn image(
    ctx: &Ctx,
    session_token: &str,
    character_id: String,
    image_id: String,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let data = {
        let _permit = ctx.app.image_decrypt_permit().await?;
        let row = sqlx::query(
            "SELECT owner_id,images FROM characters WHERE id=? AND (owner_id=? OR is_public=1)",
        )
        .bind(&character_id)
        .bind(&uid)
        .fetch_optional(ctx.db())
        .await?
        .context("character image not found")?;
        decrypt_character_images(
            &ctx.app.image_key,
            row.get("owner_id"),
            &character_id,
            row.get::<&[u8], _>("images"),
        )?
        .into_iter()
        .find(|image| image.id == image_id)
        .context("character image not found")?
        .data
    };
    ctx.respond(Response::CharacterImage {
        character_id,
        image_id,
        data,
    })
    .await
}

/// Fills portraits sent without bytes (unchanged on the client) from the
/// stored ciphertext so an edit does not need to resend every image.
async fn restore_unchanged_images(
    app: &App,
    owner_id: &str,
    character: &mut CharacterInput,
) -> Result<()> {
    let Some(id) = &character.id else {
        return Ok(());
    };
    if !character.images.iter().any(|image| image.data.is_empty()) {
        return Ok(());
    }
    let _permit = app.image_decrypt_permit().await?;
    let Some(stored) = sqlx::query_scalar::<_, Vec<u8>>("SELECT images FROM characters WHERE id=?")
        .bind(id)
        .fetch_optional(&app.db)
        .await?
    else {
        return Ok(());
    };
    let mut stored_images = decrypt_character_images(&app.image_key, owner_id, id, &stored)?;
    for image in &mut character.images {
        if image.data.is_empty()
            && let Some(stored) = stored_images
                .iter_mut()
                .find(|stored| stored.id == image.id)
        {
            image.data = std::mem::take(&mut stored.data);
        }
    }
    Ok(())
}

pub(crate) async fn upsert(
    ctx: &Ctx,
    session_token: &str,
    mut character: CharacterInput,
    world_ids: Vec<String>,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let mut operation = DeltaOperation::Add;
    if let Some(id) = &character.id {
        let owner_id: Option<String> =
            sqlx::query_scalar("SELECT owner_id FROM characters WHERE id=?")
                .bind(id)
                .fetch_optional(ctx.db())
                .await?;
        if let Some(owner_id) = owner_id {
            if owner_id != uid {
                bail!("forbidden character owner")
            }
            restore_unchanged_images(&ctx.app, &owner_id, &mut character).await?;
            operation = DeltaOperation::Update;
        }
    }
    validate_character(&character)?;
    if world_ids.len() > 128 {
        bail!("too many linked worlds")
    }
    let mut requested_world_ids = world_ids;
    requested_world_ids.sort();
    requested_world_ids.dedup();
    if character.is_public
        && user_role(ctx.db(), &uid).await? != Role::Admin
        && !load_broker_config(ctx.db()).await?.allow_public_characters
    {
        bail!("publishing characters is disabled")
    }
    // The live delta carries portrait metadata without bytes. Take the images
    // out first so cloning the character does not copy them.
    let images = std::mem::take(&mut character.images);
    let mut visible = character.clone();
    character.images = images;
    visible.owned_by_user = true;
    visible.images = character
        .images
        .iter()
        .map(|image| CharacterImage {
            id: image.id.clone(),
            label: image.label.clone(),
            data: Vec::new(),
        })
        .collect();
    let changed = encode(&DeltaPayload::Character(Box::new(visible.clone())))?;
    // Delta history is stored in SQLite, so keep licensed image bytes
    // exclusively in the authenticated ciphertext column.
    visible.avatar = None;
    visible.images.clear();
    visible.default_image_id = None;
    let stored_changed = encode(&DeltaPayload::Character(Box::new(visible)))?;
    let eid = character.id.clone().unwrap_or_else(new_uuid);

    let mut transaction = ctx.db().begin().await?;
    let character_revision = delta_tx(
        &mut transaction,
        &uid,
        "character",
        &eid,
        operation,
        &stored_changed,
    )
    .await?;
    write_character(
        &ctx.app,
        &mut transaction,
        &uid,
        &eid,
        character,
        character_revision,
    )
    .await?;
    let world_deltas =
        worlds::link_character(&mut transaction, &uid, &eid, &requested_world_ids).await?;
    transaction.commit().await?;

    ctx.delta(
        &uid,
        character_revision,
        "character",
        &eid,
        operation,
        changed,
    )
    .await?;
    let mut revision = character_revision;
    for delta in &world_deltas {
        revision = delta.revision;
        ctx.deliver(&uid, delta).await?;
    }
    ctx.accepted(Some(eid), revision).await
}

pub(crate) async fn import_from_text(
    ctx: &Ctx,
    session_token: &str,
    source_name: &str,
    text: &str,
) -> Result<()> {
    let uid = auth(ctx.db(), session_token).await?;
    let _busy = ctx.busy_guard();
    let character = import_character_from_text(&ctx.app, &uid, source_name, text).await?;
    ctx.respond(Response::CharacterImportPreview(Box::new(character)))
        .await
}
