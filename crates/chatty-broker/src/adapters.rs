//! Broker-owned adapter selection. Provider HTTP and response schemas live in WASM.
use super::*;
mod runtime;
use runtime::{Adapter, Registry};

#[derive(Clone)]
pub(super) struct AdapterHost {
    registry: Registry,
    selected_plugin: Option<String>,
    freetoken_credential: Option<(String, String)>,
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
        })
    }
    pub fn inference(&self, config: &BrokerConfig) -> Result<Adapter> {
        if !config.adapter_enabled {
            bail!("inference adapter is disabled")
        }
        self.configured_adapter(self.plugin_id(config), &config.adapter_url)
    }
    fn plugin_id(&self, config: &BrokerConfig) -> &str {
        self.selected_plugin
            .as_deref()
            .unwrap_or(if config.use_ollama_api {
                "ollama"
            } else {
                "openai-compatible"
            })
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
