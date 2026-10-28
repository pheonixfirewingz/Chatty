//! Broker-owned adapter selection. Provider HTTP and response schemas live in WASM.
use super::*;
mod runtime;
use runtime::{Adapter, Registry};
use workload::{Workload, WorkloadGate, WorkloadGuard, same_host};

#[derive(Clone)]
pub(super) struct AdapterHost {
    registry: Registry,
    selected_plugin: Option<String>,
    freetoken_credential: Option<(String, String)>,
    model_operations: ModelOperations,
    workloads: Arc<WorkloadGate>,
}
const UNLOAD_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(30)
};
const UNLOAD_POLL: Duration = Duration::from_millis(500);
type ModelOperationKey = (String, String, String);
#[derive(Clone, Default)]
struct ModelOperations(Arc<std::sync::Mutex<HashMap<ModelOperationKey, bool>>>);

pub(super) struct ModelOperationGuard {
    operations: ModelOperations,
    key: ModelOperationKey,
}
impl Drop for ModelOperationGuard {
    fn drop(&mut self) {
        if let Ok(mut operations) = self.operations.0.lock() {
            operations.remove(&self.key);
        }
    }
}

impl AdapterHost {
    pub fn from_environment() -> Result<Self> {
        let directory = std::env::var_os("CHATTY_ADAPTER_DIR").map(PathBuf::from);
        let registry = Registry::load(directory.as_deref())?;
        let selected_plugin = std::env::var("CHATTY_ADAPTER_PLUGIN")
            .ok()
            .filter(|id| !id.is_empty());
        if let Some(id) = &selected_plugin
            && !registry.contains(id)
        {
            bail!("adapter plugin not installed: {id}")
        }
        let freetoken_credential = match (
            std::env::var("CHATTY_FREETOKEN_ORIGIN").ok(),
            std::env::var("CHATTY_FREETOKEN_API_KEY").ok(),
        ) {
            (None, None) => None,
            (Some(origin), Some(key)) => {
                // Validate at startup without exposing credentials to a guest.
                Adapter::validate_bearer_token(&origin, &key)?;
                Some((origin, key))
            }
            _ => bail!("set both CHATTY_FREETOKEN_ORIGIN and CHATTY_FREETOKEN_API_KEY"),
        };
        Ok(Self {
            registry,
            selected_plugin,
            freetoken_credential,
            model_operations: ModelOperations::default(),
            workloads: Arc::default(),
        })
    }
    /// Hold for the lifetime of a text model operation, including streams.
    pub async fn text_workload(&self, config: &BrokerConfig) -> WorkloadGuard {
        let Some((guard, switch)) = self.admit(config, Workload::Text).await else {
            return WorkloadGuard::unshared();
        };
        if switch {
            // Best effort: a down image server holds no memory, so chat proceeds.
            let freed = match self.image(config) {
                Ok(adapter) => adapter.unload_image_models().await.map_err(Error::from),
                Err(error) => Err(error),
            };
            if let Err(error) = freed {
                warn!(%error, "could not free image models before text work");
            }
            self.workloads.switched();
        }
        guard
    }
    /// Hold for the lifetime of an image job. Fails rather than starting image
    /// work while a chat model may still occupy a shared GPU: image servers in
    /// another container cannot see that memory and crash when they overrun it.
    pub async fn image_workload(&self, config: &BrokerConfig) -> Result<WorkloadGuard> {
        let Some((guard, switch)) = self.admit(config, Workload::Image).await else {
            return Ok(WorkloadGuard::unshared());
        };
        if switch {
            // Dropping the guard mid-switch makes the next image job unload again.
            self.unload_text_models(config).await?;
            self.workloads.switched();
        }
        Ok(guard)
    }
    /// One GPU host cannot hold both model kinds. When both services share a
    /// host, admit one kind at a time; `true` means the caller must switch.
    async fn admit(&self, config: &BrokerConfig, kind: Workload) -> Option<(WorkloadGuard, bool)> {
        if !config.adapter_enabled
            || !config.image.enabled
            || !same_host(&config.adapter_url, &config.image.url)
        {
            return None;
        }
        Some(self.workloads.admit(kind).await)
    }
    /// Unload every running chat model, then wait until the runtime reports none.
    async fn unload_text_models(&self, config: &BrokerConfig) -> Result<()> {
        let adapter = self.management(config)?;
        let state = adapter.model_state().await.map_err(|error| {
            format_err!(
                "cannot confirm the chat model is unloaded from the shared GPU host: {error}"
            )
        })?;
        for model in state.running_models {
            info!(model = %model.name, "unloading text model for image generation");
            adapter
                .model_action(crate::adapter_api::ModelAction::Unload { model: model.name })
                .await?;
        }
        // Bounded wait, only while this switch is active.
        let deadline = Instant::now() + UNLOAD_TIMEOUT;
        loop {
            let running = adapter.model_state().await?.running_models;
            if running.is_empty() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "chat model {} is still loaded on the shared GPU host; image generation was not started",
                    running[0].name
                )
            }
            tokio::time::sleep(UNLOAD_POLL).await;
        }
    }
    pub fn reserve_model_operation(
        &self,
        config: &BrokerConfig,
        model: &str,
        download: bool,
    ) -> Result<ModelOperationGuard> {
        if model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            bail!("model name is invalid")
        }
        let plugin = self.plugin_id(config);
        let mut name = model.trim();
        if plugin == "ollama" {
            name = name.strip_prefix("registry.ollama.ai/").unwrap_or(name);
            name = name.strip_prefix("library/").unwrap_or(name);
            name = name.strip_suffix(":latest").unwrap_or(name);
        }
        let key = (
            plugin.to_owned(),
            config.adapter_url.clone(),
            name.to_owned(),
        );
        let mut operations = self
            .model_operations
            .0
            .lock()
            .map_err(|_| format_err!("model operation lock unavailable"))?;
        if operations.contains_key(&key) {
            bail!("another operation is already active for this model")
        }
        if download
            && operations.values().filter(|pull| **pull).count()
                >= chatty_protocol::admin::MAX_PARALLEL_DOWNLOADS
        {
            bail!("download limit reached (4 parallel downloads)")
        }
        operations.insert(key.clone(), download);
        Ok(ModelOperationGuard {
            operations: self.model_operations.clone(),
            key,
        })
    }
    pub fn inference(&self, config: &BrokerConfig) -> Result<Adapter> {
        if !config.adapter_enabled {
            bail!("inference adapter is disabled")
        }
        self.configured_adapter(self.plugin_id(config), &config.adapter_url)
    }
    fn plugin_id<'a>(&'a self, config: &'a BrokerConfig) -> &'a str {
        self.selected_plugin
            .as_deref()
            .unwrap_or_else(|| config.adapter_plugin_id())
    }
    pub fn validate_selection(&self, config: &BrokerConfig) -> Result<()> {
        let id = config.adapter_plugin_id();
        if !self.registry.is_inference(id) {
            bail!("inference adapter plugin not installed: {id}")
        }
        if !self.registry.is_image(&config.image.plugin) {
            bail!(
                "image adapter plugin not installed: {}",
                config.image.plugin
            )
        }
        Ok(())
    }
    pub fn config_response(&self, config: BrokerConfig) -> Response {
        Response::BrokerConfig {
            config: Box::new(config),
            adapter_plugins: self.registry.inference_plugin_ids(),
            adapter_override: self.selected_plugin.clone(),
            image_plugins: self.registry.image_plugin_ids(),
        }
    }
    pub fn configuration_changed(&self, config: &BrokerConfig) -> Result<()> {
        Ok(self
            .registry
            .select(config.adapter_enabled.then(|| self.plugin_id(config)))?)
    }
    pub fn management(&self, config: &BrokerConfig) -> Result<Adapter> {
        self.configured_adapter(self.plugin_id(config), &config.adapter_url)
    }
    fn configured_adapter(&self, id: &str, url: &str) -> Result<Adapter> {
        let adapter = self.registry.selected_adapter(id, url)?;
        if id == "freetoken"
            && let Some((origin, key)) = &self.freetoken_credential
        {
            return Ok(adapter.with_bearer_token(origin, key)?);
        }
        Ok(adapter)
    }
    /// Image adapters are transient: jobs are rare, and no module is retained.
    pub fn image(&self, config: &BrokerConfig) -> Result<Adapter> {
        let image = &config.image;
        if !image.enabled {
            bail!("image generation is disabled")
        }
        if !self.registry.is_image(&image.plugin) {
            bail!("image adapter plugin not installed: {}", image.plugin)
        }
        Ok(self.registry.adapter(&image.plugin, &image.url)?)
    }
    pub fn catalogue(&self) -> Result<Adapter> {
        Ok(self
            .registry
            .adapter("hugging-face", "https://huggingface.co")?)
    }
}

#[cfg(test)]
pub(super) fn test_adapter_host() -> AdapterHost {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    AdapterHost {
        registry: REGISTRY
            .get_or_init(|| Registry::bundled().expect("bundled WASM adapters"))
            .clone(),
        selected_plugin: None,
        freetoken_credential: None,
        model_operations: ModelOperations::default(),
        workloads: Arc::default(),
    }
}

pub(super) fn adapter_token_usage(usage: crate::adapter_api::TokenUsage) -> TokenUsage {
    TokenUsage {
        prompt_tokens: usage.prompt_tokens,
        completion_tokens: usage.completion_tokens,
    }
}

pub(super) async fn complete(
    app: &App,
    model: &str,
    messages: Vec<AdapterMessage>,
    options: GenerationOptions,
    timeout: Duration,
) -> Result<Completion> {
    let config = load_broker_config(&app.db).await?;
    let _workload = app.adapters.text_workload(&config).await;
    Ok(app
        .adapters
        .inference(&config)?
        .complete(ChatRequest {
            model: model.into(),
            messages,
            options,
            timeout_ms: Some(timeout.as_millis().min(u128::from(u64::MAX)) as u64),
        })
        .await?)
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[test]
    fn freetoken_selection_rejects_configured_origin_changes() {
        let mut host = test_adapter_host();
        host.selected_plugin = Some("freetoken".into());
        host.freetoken_credential = Some((
            "https://inference.example:1919".into(),
            "synthetic-broker-credential".into(),
        ));
        assert!(
            host.configured_adapter("freetoken", "https://inference.example:1919/v1")
                .is_ok()
        );
        for url in [
            "https://elsewhere.example:1919/v1",
            "http://inference.example:1919/v1",
            "https://inference.example:1920/v1",
        ] {
            assert!(host.configured_adapter("freetoken", url).is_err());
        }
        // Other providers and the catalogue do not use the FreeToken credential.
        assert!(
            host.configured_adapter("openai-compatible", "http://other.example/v1")
                .is_ok()
        );
        assert!(
            host.configured_adapter("ollama", "http://other.example/v1")
                .is_ok()
        );
        assert!(host.catalogue().is_ok());
    }
}
