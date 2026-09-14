use super::*;

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
