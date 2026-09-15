use super::*;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const IMAGE_CIPHERTEXT_MAGIC: &[u8] = b"CHIMG1";

pub(super) fn load_or_create_image_key(tls_key_path: &str) -> Result<[u8; 32]> {
    let path = PathBuf::from(tls_key_path).with_extension("image-key");
    if path.exists() {
        let mut key = [0u8; 32];
        File::open(&path)
            .with_context(|| format!("open image encryption key {}", path.display()))?
            .read_exact(&mut key)
            .with_context(|| format!("read image encryption key {}", path.display()))?;
        return Ok(key);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)
        .map_err(|error| format_err!("generate image encryption key: {error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(&path) {
        Ok(mut file) => file
            .write_all(&key)
            .with_context(|| format!("write image encryption key {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            File::open(&path)
                .with_context(|| format!("open image encryption key {}", path.display()))?
                .read_exact(&mut key)
                .with_context(|| format!("read image encryption key {}", path.display()))?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("create image encryption key {}", path.display()));
        }
    }
    Ok(key)
}

pub(super) fn encrypt_character_images(
    key: &[u8; 32],
    owner_id: &str,
    character_id: &str,
    images: &[CharacterImage],
) -> Result<Vec<u8>> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    let plaintext = encode(&images.to_vec())?;
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce)
        .map_err(|error| format_err!("generate image encryption nonce: {error}"))?;
    let aad = format!("{owner_id}:{character_id}");
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| format_err!("encrypt character images"))?;
    let mut stored = Vec::with_capacity(IMAGE_CIPHERTEXT_MAGIC.len() + nonce.len() + ciphertext.len());
    stored.extend_from_slice(IMAGE_CIPHERTEXT_MAGIC);
    stored.extend_from_slice(&nonce);
    stored.extend_from_slice(&ciphertext);
    Ok(stored)
}

pub(super) fn decrypt_character_images(
    key: &[u8; 32],
    owner_id: &str,
    character_id: &str,
    stored: &[u8],
) -> Result<Vec<CharacterImage>> {
    if stored.is_empty() {
        return Ok(Vec::new());
    }
    let encrypted = stored
        .strip_prefix(IMAGE_CIPHERTEXT_MAGIC)
        .context("character images are not encrypted")?;
    if encrypted.len() < 24 {
        bail!("character image ciphertext is truncated")
    }
    let (nonce, ciphertext) = encrypted.split_at(24);
    let aad = format!("{owner_id}:{character_id}");
    let plaintext = XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| format_err!("decrypt character images"))?;
    Ok(decode(&plaintext)?)
}

pub(super) fn default_user_data_dir(
    xdg_data_home: Option<PathBuf>,
    user_home: Option<PathBuf>,
) -> Option<PathBuf> {
    xdg_data_home
        .filter(|path| path.is_absolute())
        .or_else(|| {
            user_home
                .filter(|path| path.is_absolute())
                .map(|path| path.join(".local/share"))
        })
        .map(|path| path.join("chatty"))
}

pub(super) fn default_database_url() -> Result<String> {
    let data_dir = default_user_data_dir(
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .context(
        "could not determine the Linux user data directory; set XDG_DATA_HOME, HOME, or CHATTY_DATABASE",
    )?;
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("create application data directory {}", data_dir.display()))?;
    Ok(format!(
        "sqlite://{}?mode=rwc",
        data_dir.join("chatty.db").display()
    ))
}

pub(super) fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(super) fn delta_visible(
    identity: Option<&str>,
    connection_id: &str,
    event: &PublishedDelta,
) -> bool {
    event.origin != connection_id && identity == Some(event.owner_id.as_str())
}

pub(super) fn classify_error(error: &Error) -> WireError {
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

pub(super) fn is_mutating(request: &Request) -> bool {
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
    )
}

pub(super) fn validate_credentials(u: &str, p: &str) -> Result<()> {
    if u.len() < 3 || u.len() > 64 || p.len() < 10 || p.len() > 1024 {
        bail!("username or password length invalid")
    }
    Ok(())
}
pub(super) fn validate_character(character: &CharacterInput) -> Result<()> {
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

pub(super) fn clip(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub(super) fn token_usage_from_openai(value: &Value) -> TokenUsage {
    TokenUsage {
        prompt_tokens: value["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        completion_tokens: value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    }
}

pub(super) async fn record_token_usage(app: &App, user_id: &str, usage: TokenUsage) -> Result<()> {
    if usage.total() == 0 {
        return Ok(());
    }
    sqlx::query("UPDATE users SET prompt_tokens=prompt_tokens+?,completion_tokens=completion_tokens+? WHERE id=?")
        .bind(i64::try_from(usage.prompt_tokens).context("prompt token count overflow")?)
        .bind(i64::try_from(usage.completion_tokens).context("completion token count overflow")?)
        .bind(user_id)
        .execute(&app.db)
        .await?;
    Ok(())
}

pub(super) fn drain_sse(
    buffer: &mut Vec<u8>,
    pending: &mut String,
    complete: &mut String,
    usage: &mut TokenUsage,
) -> Result<bool> {
    let mut consumed = 0;
    let mut done = false;
    while let Some(relative_end) = buffer[consumed..].iter().position(|b| *b == b'\n') {
        let end = consumed + relative_end;
        let line = std::str::from_utf8(&buffer[consumed..end])?.trim_end_matches('\r');
        consumed = end + 1;
        let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
            continue;
        };
        if data == "[DONE]" {
            done = true;
            break;
        }
        let value: Value =
            serde_json::from_str(data).context("malformed llama-server SSE event")?;
        if let Some(value_usage) = value.get("usage") {
            usage.prompt_tokens = value_usage["prompt_tokens"]
                .as_u64()
                .unwrap_or(usage.prompt_tokens);
            usage.completion_tokens = value_usage["completion_tokens"]
                .as_u64()
                .unwrap_or(usage.completion_tokens);
        }
        if let Some(text) = value["choices"][0]["delta"]["content"].as_str() {
            pending.push_str(text);
            complete.push_str(text);
        }
    }
    buffer.drain(..consumed);
    Ok(done)
}

pub(super) fn drain_ollama_stream(
    buffer: &mut Vec<u8>,
    pending: &mut String,
    complete: &mut String,
    usage: &mut TokenUsage,
) -> Result<bool> {
    let mut consumed = 0;
    let mut done = false;
    while let Some(relative_end) = buffer[consumed..].iter().position(|byte| *byte == b'\n') {
        let end = consumed + relative_end;
        let line = std::str::from_utf8(&buffer[consumed..end])?.trim();
        consumed = end + 1;
        if line.is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(line).context("malformed Ollama NDJSON stream event")?;
        if let Some(error) = value["error"].as_str() {
            bail!("Ollama generation failed: {error}")
        }
        if let Some(text) = value["message"]["content"].as_str() {
            pending.push_str(text);
            complete.push_str(text);
        }
        usage.prompt_tokens = value["prompt_eval_count"]
            .as_u64()
            .unwrap_or(usage.prompt_tokens);
        usage.completion_tokens = value["eval_count"]
            .as_u64()
            .unwrap_or(usage.completion_tokens);
        done |= value["done"].as_bool().unwrap_or(false);
        if done {
            break;
        }
    }
    buffer.drain(..consumed);
    Ok(done)
}
