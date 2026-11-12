//! Broker configuration storage and validation.
use super::*;

pub(crate) async fn load_broker_config(db: &SqlitePool) -> Result<BrokerConfig> {
    let row = sqlx::query("SELECT adapter_enabled,adapter_url,adapter_plugin,use_ollama_api,model,temperature,top_p,top_k,num_ctx,num_predict,repeat_penalty,seed,keep_alive,allow_public_characters,allow_self_registration,image_enabled,image_url,image_plugin,image_model,image_width,image_height,image_steps,image_guidance,image_negative_prompt,image_workflow,emotion_model,reasoning,memory_model FROM broker_settings WHERE singleton=1")
        .fetch_one(db)
        .await?;
    Ok(BrokerConfig {
        adapter_enabled: row.get("adapter_enabled"),
        adapter_url: row.get("adapter_url"),
        adapter_plugin: row.get("adapter_plugin"),
        use_ollama_api: row.get("use_ollama_api"),
        model: row.get("model"),
        temperature: row.get("temperature"),
        top_p: row.get("top_p"),
        top_k: row.get::<i64, _>("top_k") as u32,
        num_ctx: row.get::<i64, _>("num_ctx") as u32,
        num_predict: row.get::<i64, _>("num_predict") as i32,
        repeat_penalty: row.get("repeat_penalty"),
        seed: row.get("seed"),
        keep_alive: row.get("keep_alive"),
        allow_public_characters: row.get("allow_public_characters"),
        allow_self_registration: row.get("allow_self_registration"),
        image: ImageConfig {
            enabled: row.get("image_enabled"),
            url: row.get("image_url"),
            plugin: row.get("image_plugin"),
            model: row.get("image_model"),
            width: row.get::<i64, _>("image_width") as u32,
            height: row.get::<i64, _>("image_height") as u32,
            steps: row.get::<i64, _>("image_steps") as u32,
            guidance: row.get("image_guidance"),
            negative_prompt: row.get("image_negative_prompt"),
            workflow: row.get("image_workflow"),
        },
        emotion_model: row.get("emotion_model"),
        memory_model: row.get("memory_model"),
        // The CHECK constraint keeps stored names valid.
        reasoning: ReasoningLevel::from_name(row.get("reasoning")).unwrap_or_default(),
    })
}

pub(crate) fn validate_broker_config(config: &BrokerConfig) -> Result<()> {
    if config.model.len() > 256 {
        bail!("model name must not exceed 256 bytes")
    }
    if !config.temperature.is_finite() || !(0.0..=2.0).contains(&config.temperature) {
        bail!("temperature must be between 0 and 2")
    }
    if !config.top_p.is_finite() || !(0.0..=1.0).contains(&config.top_p) {
        bail!("top-p must be between 0 and 1")
    }
    if config.top_k > 10_000 {
        bail!("top-k must be between 0 and 10000")
    }
    if !(128..=1_048_576).contains(&config.num_ctx) {
        bail!("context length must be between 128 and 1048576")
    }
    if config.num_predict < -1 || config.num_predict > 1_048_576 {
        bail!("prediction limit must be -1 or between 0 and 1048576")
    }
    if !config.repeat_penalty.is_finite() || !(0.0..=2.0).contains(&config.repeat_penalty) {
        bail!("repeat penalty must be between 0 and 2")
    }
    if config.keep_alive.is_empty() || config.keep_alive.len() > 32 {
        bail!("keep-alive must be an Ollama duration such as 5m, 1h, or 0")
    }
    if config.emotion_model.len() > 4096 || config.emotion_model.chars().any(char::is_control) {
        bail!("emotion model path is invalid")
    }
    if config.memory_model.len() > 256 || config.memory_model.chars().any(char::is_control) {
        bail!("memory model name is invalid")
    }
    let image = &config.image;
    if !image.url.starts_with("http://") && !image.url.starts_with("https://") {
        bail!("image service URL must use http or https")
    }
    if image.model.len() > 256 || image.model.chars().any(char::is_control) {
        bail!("image model name is invalid")
    }
    let sides = chatty_protocol::admin::MIN_IMAGE_SIDE..=chatty_protocol::admin::MAX_IMAGE_SIDE;
    if !sides.contains(&image.width)
        || !sides.contains(&image.height)
        || !image.width.is_multiple_of(8)
        || !image.height.is_multiple_of(8)
    {
        bail!("image size must be 256 to 2048 pixels per side, in multiples of 8")
    }
    if !(1..=chatty_protocol::admin::MAX_IMAGE_STEPS).contains(&image.steps) {
        bail!("image steps must be between 1 and 150")
    }
    if !image.guidance.is_finite() || !(0.0..=30.0).contains(&image.guidance) {
        bail!("image guidance must be between 0 and 30")
    }
    if image.negative_prompt.len() > chatty_protocol::admin::MAX_IMAGE_PROMPT_BYTES {
        bail!("image negative prompt must not exceed 4096 bytes")
    }
    validate_image_workflow(&image.workflow)
}

/// Check a ComfyUI API-format template before it is saved, so mistakes surface
/// in the admin portal instead of during a user's image request.
pub(crate) fn validate_image_workflow(workflow: &str) -> Result<()> {
    if workflow.is_empty() {
        return Ok(());
    }
    if workflow.len() > chatty_protocol::admin::MAX_IMAGE_WORKFLOW_BYTES {
        bail!("image workflow must not exceed 256 KiB")
    }
    let graph: Value =
        serde_json::from_str(workflow).context("image workflow is not valid JSON")?;
    let Some(nodes) = graph.as_object() else {
        bail!("image workflow must be a JSON object of nodes")
    };
    if nodes.contains_key("nodes") && nodes.contains_key("links") {
        bail!("this is a UI workflow; in ComfyUI use Workflow > Export (API)")
    }
    if nodes.is_empty() || nodes.len() > 500 {
        bail!("image workflow must contain 1 to 500 nodes")
    }
    for (id, node) in nodes {
        if !node["class_type"].is_string() || !node["inputs"].is_object() {
            bail!("workflow node {id} needs a class_type and inputs")
        }
    }
    if !workflow.contains("{{prompt}}") {
        bail!("image workflow must use {{{{prompt}}}} in a text input")
    }
    let mut rest = workflow;
    while let Some(start) = rest.find("{{") {
        let Some(length) = rest[start..].find("}}") else {
            break;
        };
        let placeholder = &rest[start..start + length + 2];
        if !chatty_protocol::admin::IMAGE_WORKFLOW_PLACEHOLDERS.contains(&placeholder) {
            bail!("unknown image workflow placeholder {placeholder}")
        }
        rest = &rest[start + length + 2..];
    }
    Ok(())
}

/// Normalizes, validates, and stores a new broker configuration, then
/// applies it to the adapter host. Returns the stored configuration.
pub(crate) async fn save_broker_config(
    app: &App,
    mut config: BrokerConfig,
) -> Result<BrokerConfig> {
    config.adapter_url = config.adapter_url.trim().trim_end_matches('/').to_owned();
    config.model = config.model.trim().to_owned();
    config.keep_alive = config.keep_alive.trim().to_owned();
    config.image.url = config.image.url.trim().trim_end_matches('/').to_owned();
    config.image.model = config.image.model.trim().to_owned();
    config.image.negative_prompt = config.image.negative_prompt.trim().to_owned();
    config.emotion_model = config.emotion_model.trim().to_owned();
    config.memory_model = config.memory_model.trim().to_owned();
    if !config.adapter_url.starts_with("http://") && !config.adapter_url.starts_with("https://") {
        bail!("adapter URL must use http or https")
    }
    validate_broker_config(&config)?;
    if !config.emotion_model.is_empty() && !Path::new(&config.emotion_model).is_file() {
        bail!("emotion model must be a GGUF file path on the broker host")
    }
    app.adapters.validate_selection(&config)?;
    sqlx::query("UPDATE broker_settings SET adapter_enabled=?,adapter_url=?,adapter_plugin=?,use_ollama_api=?,model=?,temperature=?,top_p=?,top_k=?,num_ctx=?,num_predict=?,repeat_penalty=?,seed=?,keep_alive=?,allow_public_characters=?,allow_self_registration=?,image_enabled=?,image_url=?,image_plugin=?,image_model=?,image_width=?,image_height=?,image_steps=?,image_guidance=?,image_negative_prompt=?,image_workflow=?,emotion_model=?,reasoning=?,memory_model=?,updated_at=CURRENT_TIMESTAMP WHERE singleton=1")
        .bind(config.adapter_enabled)
        .bind(&config.adapter_url)
        .bind(&config.adapter_plugin)
        .bind(config.use_ollama_api)
        .bind(&config.model)
        .bind(config.temperature)
        .bind(config.top_p)
        .bind(config.top_k)
        .bind(config.num_ctx)
        .bind(config.num_predict)
        .bind(config.repeat_penalty)
        .bind(config.seed)
        .bind(&config.keep_alive)
        .bind(config.allow_public_characters)
        .bind(config.allow_self_registration)
        .bind(config.image.enabled)
        .bind(&config.image.url)
        .bind(&config.image.plugin)
        .bind(&config.image.model)
        .bind(config.image.width)
        .bind(config.image.height)
        .bind(config.image.steps)
        .bind(config.image.guidance)
        .bind(&config.image.negative_prompt)
        .bind(&config.image.workflow)
        .bind(&config.emotion_model)
        .bind(config.reasoning.as_str())
        .bind(&config.memory_model)
        .execute(&app.db)
        .await?;
    app.adapters.configuration_changed(&config)?;
    Ok(config)
}
