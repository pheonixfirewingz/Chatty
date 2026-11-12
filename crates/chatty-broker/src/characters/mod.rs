//! Character storage: validation, row decoding, and encrypted portraits.
use super::*;

pub(crate) mod handlers;

const CHARACTER_UPSERT: &str = "INSERT INTO characters(id,owner_id,name,description,personality,scenario,system_prompt,example_dialogue,appearance,age,gender,race,misc,tags,avatar,images,default_image_id,revision,is_public) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,description=excluded.description,personality=excluded.personality,scenario=excluded.scenario,system_prompt=excluded.system_prompt,example_dialogue=excluded.example_dialogue,appearance=excluded.appearance,age=excluded.age,gender=excluded.gender,race=excluded.race,misc=excluded.misc,tags=excluded.tags,avatar=excluded.avatar,images=excluded.images,default_image_id=excluded.default_image_id,revision=excluded.revision,is_public=excluded.is_public WHERE owner_id=excluded.owner_id";

/// Decodes a `characters` row for `user_id`. Portrait metadata is kept and
/// image bytes are dropped; clients fetch bytes on demand.
pub(crate) async fn character_from_row(
    app: &App,
    row: &sqlx::sqlite::SqliteRow,
    user_id: &str,
) -> Result<CharacterInput> {
    let id: String = row.get("id");
    let owner_id: &str = row.get("owner_id");
    let stored = row.get::<&[u8], _>("images");
    let mut images = if stored.is_empty() {
        Vec::new()
    } else {
        let _permit = app.image_decrypt_permit().await?;
        decrypt_character_images(&app.image_key, owner_id, &id, stored)?
    };
    for image in &mut images {
        image.data = Vec::new();
    }
    Ok(CharacterInput {
        name: row.get("name"),
        description: row.get("description"),
        personality: row.get("personality"),
        scenario: row.get("scenario"),
        system_prompt: row.get("system_prompt"),
        example_dialogue: row.get("example_dialogue"),
        appearance: row.get("appearance"),
        age: row.get("age"),
        gender: row.get("gender"),
        race: row.get("race"),
        misc: row.get("misc"),
        tags: decode(row.get::<&[u8], _>("tags")).unwrap_or_default(),
        avatar: row.get("avatar"),
        images,
        default_image_id: row.get("default_image_id"),
        is_public: row.get("is_public"),
        owned_by_user: owner_id == user_id,
        id: Some(id),
    })
}

/// Inserts or updates a character the caller owns inside `transaction`.
/// Portrait bytes go only to the encrypted column; the plain avatar is kept
/// only when there are no portraits.
pub(crate) async fn write_character(
    app: &App,
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    owner_id: &str,
    id: &str,
    character: CharacterInput,
    revision: i64,
) -> Result<()> {
    let images = encrypt_character_images(&app.image_key, owner_id, id, &character.images)?;
    let avatar = if character.images.is_empty() {
        character.avatar
    } else {
        None
    };
    sqlx::query(CHARACTER_UPSERT)
        .bind(id)
        .bind(owner_id)
        .bind(character.name)
        .bind(character.description)
        .bind(character.personality)
        .bind(character.scenario)
        .bind(character.system_prompt)
        .bind(character.example_dialogue)
        .bind(character.appearance)
        .bind(character.age)
        .bind(character.gender)
        .bind(character.race)
        .bind(character.misc)
        .bind(encode(&character.tags)?)
        .bind(avatar)
        .bind(images)
        .bind(character.default_image_id)
        .bind(revision)
        .bind(character.is_public)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// The character used when a user starts a direct chat without choosing one.
pub(crate) fn default_assistant(id: String) -> CharacterInput {
    CharacterInput {
        id: Some(id),
        name: "Assistant".into(),
        description: String::new(),
        personality: "Helpful, attentive, and conversational.".into(),
        scenario: String::new(),
        system_prompt: "Respond naturally and remain consistent with the conversation.".into(),
        example_dialogue: String::new(),
        appearance: String::new(),
        age: String::new(),
        gender: String::new(),
        race: String::new(),
        misc: String::new(),
        tags: vec!["default".into()],
        avatar: None,
        images: vec![],
        default_image_id: None,
        is_public: false,
        owned_by_user: true,
    }
}

/// True when `user_id` owns the character or it is public.
pub(crate) async fn character_accessible<'e>(
    executor: impl sqlx::SqliteExecutor<'e>,
    user_id: &str,
    character_id: &str,
) -> Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM characters WHERE id=? AND (owner_id=? OR is_public=1))",
    )
    .bind(character_id)
    .bind(user_id)
    .fetch_one(executor)
    .await?)
}

pub(crate) fn validate_character(character: &CharacterInput) -> Result<()> {
    if character.name.is_empty() || character.name.len() > 256 {
        bail!("character name length invalid")
    }
    for value in [
        &character.description,
        &character.personality,
        &character.scenario,
        &character.system_prompt,
        &character.example_dialogue,
        &character.appearance,
        &character.age,
        &character.gender,
        &character.race,
        &character.misc,
    ] {
        if value.len() > 65_536 {
            bail!("character field too large")
        }
    }
    if character.tags.len() > 128 || character.tags.iter().any(|tag| tag.len() > 256) {
        bail!("character tags too large")
    }
    if character
        .avatar
        .as_ref()
        .is_some_and(|avatar| avatar.len() > 2 * 1024 * 1024)
    {
        bail!("character avatar too large")
    }
    if character.images.len() > 32 {
        bail!("too many character images")
    }
    let mut total_image_bytes = 0usize;
    let mut image_ids = std::collections::HashSet::new();
    for image in &character.images {
        if image.id.is_empty()
            || image.id.len() > 128
            || !image_ids.insert(image.id.as_str())
            || image.label.trim().is_empty()
            || image.label.len() > 256
            || image.data.len() > 2 * 1024 * 1024
        {
            bail!("character image invalid")
        }
        let (width, height) = image::ImageReader::with_format(
            std::io::Cursor::new(&image.data),
            image::ImageFormat::Png,
        )
        .into_dimensions()
        .map_err(|_| format_err!("character image must be a valid PNG"))?;
        if width < 128 || height < 128 || width > 512 || height > 512 {
            bail!("character image dimensions must be between 128x128 and 512x512")
        }
        total_image_bytes = total_image_bytes.saturating_add(image.data.len());
    }
    // Keep the complete compressed request below the protocol's 8 MiB frame
    // ceiling even when the default avatar is also present.
    if total_image_bytes > 5 * 1024 * 1024 {
        bail!("character images too large")
    }
    if let Some(default_id) = &character.default_image_id
        && !character.images.iter().any(|image| &image.id == default_id)
    {
        bail!("default character image not found")
    }
    Ok(())
}
