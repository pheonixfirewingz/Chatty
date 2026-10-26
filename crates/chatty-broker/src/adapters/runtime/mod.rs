//! Host for import-free WASM API translators.
//!
//! HTTP, TLS, timeouts, backpressure, and cancellation remain host-owned. A guest
//! can only describe calls to its configured origin; redirects are disabled.
mod error;
mod wasm;

pub use crate::adapter_api::*;
pub use error::{Error, Result};
use futures_util::{Stream, StreamExt, future::try_join_all};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use wasm::{Plugin, Session};

/// Discovery stores only module locations, never compiled guest modules.
enum PluginSource {
    Bundled(&'static [u8]),
    File(PathBuf),
}
impl PluginSource {
    fn load(&self) -> Result<Plugin> {
        match self {
            Self::Bundled(bytes) => Plugin::load(bytes),
            Self::File(path) => Plugin::load(&bounded_file(path, wasm::MAX_WASM_BYTES)?),
        }
    }
}

#[derive(Default)]
struct SelectedPlugin {
    id: Option<String>,
    module: Option<Plugin>,
}

#[derive(Clone)]
pub struct Registry {
    plugins: Arc<HashMap<String, PluginSource>>,
    selected: Arc<Mutex<SelectedPlugin>>,
    client: reqwest::Client,
}
impl Registry {
    #[cfg(test)]
    pub fn bundled() -> Result<Self> {
        Self::load(None)
    }

    /// Load bundled plugins and optional <directory>/<plugin>/adapter.json entries.
    /// Startup validates manifests only. WASM is read and compiled on demand.
    pub fn load(directory: Option<&Path>) -> Result<Self> {
        let mut plugins = HashMap::new();
        for (id, bytes) in [
            (
                "openai-compatible",
                include_bytes!("../../../../chatty-adapters/openai-compatible/adapter.wasm")
                    .as_slice(),
            ),
            (
                "freetoken",
                include_bytes!("../../../../chatty-adapters/freetoken/adapter.wasm").as_slice(),
            ),
            (
                "ollama",
                include_bytes!("../../../../chatty-adapters/ollama/adapter.wasm").as_slice(),
            ),
            (
                "hugging-face",
                include_bytes!("../../../../chatty-adapters/hugging-face/adapter.wasm").as_slice(),
            ),
        ] {
            plugins.insert(id.into(), PluginSource::Bundled(bytes));
        }
        if let Some(directory) = directory {
            let mut entries = std::fs::read_dir(directory)?
                .take(65)
                .collect::<std::io::Result<Vec<_>>>()?;
            if entries.len() > 64 {
                return Err(Error::new("adapter directory exceeds 64 entries"));
            }
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                let path = entry.path();
                let manifest = bounded_file(&path.join("adapter.json"), 16 * 1024)?;
                let manifest: Value = serde_json::from_slice(&manifest)?;
                let id = manifest["id"]
                    .as_str()
                    .ok_or_else(|| Error::new("adapter manifest missing id"))?;
                if id.is_empty()
                    || id.len() > 64
                    || !id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
                {
                    return Err(Error::new("invalid adapter plugin id"));
                }
                if manifest["api_version"].as_u64() != Some(u64::from(ABI_VERSION)) {
                    return Err(Error::new(format!(
                        "unsupported manifest API version for {id}"
                    )));
                }
                if plugins.contains_key(canonical_plugin_id(id)) {
                    return Err(Error::new(format!("duplicate adapter plugin id: {id}")));
                }
                let filename = manifest["module"]
                    .as_str()
                    .ok_or_else(|| Error::new("adapter manifest missing module"))?;
                if filename.is_empty()
                    || filename.contains(['/', '\\'])
                    || matches!(filename, "." | "..")
                {
                    return Err(Error::new(
                        "adapter module must be a filename within its plugin directory",
                    ));
                }
                plugins.insert(id.into(), PluginSource::File(path.join(filename)));
            }
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            plugins: Arc::new(plugins),
            selected: Arc::new(Mutex::new(SelectedPlugin::default())),
            client,
        })
    }
    /// Set the administrator's selection without loading a module. Evict any old
    /// cached module immediately; active requests retain only their own references.
    pub fn select(&self, id: Option<&str>) -> Result<()> {
        let id = id.map(canonical_plugin_id);
        if let Some(id) = id
            && !self.contains(id)
        {
            return Err(Error::new(format!("adapter plugin not installed: {id}")));
        }
        let mut selected = self.selected.lock().map_err(Error::new)?;
        if selected.id.as_deref() != id {
            selected.module = None;
            selected.id = id.map(str::to_owned);
        }
        Ok(())
    }

    fn load_plugin(&self, id: &str) -> Result<Plugin> {
        self.plugins
            .get(canonical_plugin_id(id))
            .ok_or_else(|| Error::new(format!("adapter plugin not installed: {id}")))?
            .load()
    }

    /// Transient adapter: no registry reference remains once its operation ends.
    pub fn adapter(&self, id: &str, base_url: &str) -> Result<Adapter> {
        self.make_adapter(id, base_url, false)
    }

    /// Cache only the current administrator-selected adapter. Requests which
    /// captured a previous selection can finish, but cannot repopulate its cache.
    pub fn selected_adapter(&self, id: &str, base_url: &str) -> Result<Adapter> {
        self.make_adapter(id, base_url, true)
    }

    fn make_adapter(&self, id: &str, base_url: &str, retain: bool) -> Result<Adapter> {
        let base = reqwest::Url::parse(base_url).map_err(Error::new)?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(Error::new("adapter URL must use HTTP or HTTPS"));
        }
        let id = canonical_plugin_id(id);
        // Catalogue code stays transient even if a caller selects its ID directly.
        let retain = retain && id != "hugging-face";
        let plugin = if retain {
            let mut selected = self.selected.lock().map_err(Error::new)?;
            if selected.id.as_deref() == Some(id) {
                if selected.module.is_none() {
                    selected.module = Some(self.load_plugin(id)?);
                }
                selected
                    .module
                    .as_ref()
                    .expect("selected module loaded")
                    .clone()
            } else {
                drop(selected);
                self.load_plugin(id)?
            }
        } else {
            self.load_plugin(id)?
        };
        Ok(Adapter {
            plugin,
            client: self.client.clone(),
            base,
            credential: None,
        })
    }
    pub fn contains(&self, id: &str) -> bool {
        self.plugins.contains_key(canonical_plugin_id(id))
    }
}

// Preserve existing operator selections while exposing the clearer canonical ID.
fn canonical_plugin_id(id: &str) -> &str {
    if id == "openai" {
        "openai-compatible"
    } else {
        id
    }
}

fn bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::new(format!(
            "adapter file exceeds {limit} bytes: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

#[derive(Clone)]
pub struct Adapter {
    plugin: Plugin,
    client: reqwest::Client,
    base: reqwest::Url,
    credential: Option<BearerCredential>,
}

#[derive(Clone)]
struct BearerCredential {
    origin: reqwest::Url,
    header: reqwest::header::HeaderValue,
}
fn redact_error(error: Error, credential: Option<&BearerCredential>) -> Error {
    let Some(credential) = credential else {
        return error;
    };
    let key = credential
        .header
        .to_str()
        .expect("validated header")
        .strip_prefix("Bearer ")
        .expect("bearer header");
    Error::new(error.to_string().replace(key, "[redacted]"))
}

fn bearer_credential(base: &reqwest::Url, origin: &str, token: &str) -> Result<BearerCredential> {
    let origin =
        reqwest::Url::parse(origin).map_err(|_| Error::new("invalid credential origin"))?;
    if !matches!(origin.scheme(), "http" | "https")
        || origin.host_str().is_none()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || base.origin() != origin.origin()
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err(Error::new(
            "credential origin must match the configured server origin, without paths or URL credentials",
        ));
    }
    if token.is_empty() {
        return Err(Error::new("adapter API key must not be empty"));
    }
    let mut header = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Error::new("invalid adapter API key header"))?;
    header
        .to_str()
        .map_err(|_| Error::new("adapter API key must use ASCII header characters"))?;
    header.set_sensitive(true);
    Ok(BearerCredential { origin, header })
}

impl Adapter {
    /// Attach a host-owned key pinned to an explicit operator-configured origin.
    /// Neither the key nor its header is passed into guest memory.
    pub fn with_bearer_token(mut self, origin: &str, token: &str) -> Result<Self> {
        self.credential = Some(bearer_credential(&self.base, origin, token)?);
        Ok(self)
    }
    pub fn validate_bearer_token(origin: &str, token: &str) -> Result<()> {
        let base =
            reqwest::Url::parse(origin).map_err(|_| Error::new("invalid credential origin"))?;
        bearer_credential(&base, origin, token)?;
        Ok(())
    }
    fn prepare(&self, session: &mut Session, request: AdapterRequest) -> Result<HttpPlan> {
        // Guest plans cannot turn an inference/read capability into a lifecycle mutation,
        // change the expected result type, or lengthen the caller's time budget.
        let (kind, stream, timeout, methods, max_requests, response_limit) = match &request {
            AdapterRequest::Models { timeout_ms } => (
                ResponseKind::Models,
                false,
                Some(*timeout_ms),
                vec![HttpMethod::Get],
                1,
                MAX_MESSAGE_BYTES,
            ),
            AdapterRequest::Complete(chat) => (
                ResponseKind::Completion,
                false,
                chat.timeout_ms.or(Some(6 * 60 * 60 * 1000)),
                vec![HttpMethod::Post],
                1,
                MAX_MESSAGE_BYTES,
            ),
            AdapterRequest::Stream(chat) => (
                ResponseKind::Completion,
                true,
                chat.timeout_ms,
                vec![HttpMethod::Post],
                1,
                MAX_MESSAGE_BYTES,
            ),
            AdapterRequest::State => (
                ResponseKind::State,
                false,
                Some(20_000),
                vec![HttpMethod::Get],
                4,
                MAX_MESSAGE_BYTES,
            ),
            AdapterRequest::Action(_) => (
                ResponseKind::Action,
                false,
                Some(6 * 60 * 60 * 1000),
                vec![HttpMethod::Post, HttpMethod::Delete],
                1,
                MAX_MESSAGE_BYTES,
            ),
            AdapterRequest::Search { .. } => (
                ResponseKind::Search,
                false,
                Some(20_000),
                vec![HttpMethod::Get],
                1,
                2 * 1024 * 1024,
            ),
        };
        let output = session.invoke(&PluginInput::Prepare {
            base_url: self.base.to_string().trim_end_matches('/').into(),
            request,
        })?;
        let PluginOutput::Plan(mut plan) = output else {
            return Err(Error::new("adapter returned no HTTP plan"));
        };
        validate_plan(&self.base, &plan)?;
        if plan.response != kind || plan.stream != stream || plan.requests.len() > max_requests {
            return Err(Error::new(
                "adapter HTTP plan does not match the requested operation",
            ));
        }
        plan.response_limit = plan.response_limit.min(response_limit);
        for request in &mut plan.requests {
            if !methods.contains(&request.method) {
                return Err(Error::new(
                    "adapter HTTP method is not allowed for this operation",
                ));
            }
            if let Some(timeout) = timeout {
                request.timeout_ms = Some(request.timeout_ms.unwrap_or(timeout).min(timeout));
            }
        }
        Ok(plan)
    }
    async fn send(&self, request: HttpRequest) -> Result<reqwest::Response> {
        // Check again at the transport boundary before attaching credentials.
        let url = reqwest::Url::parse(&request.url).map_err(Error::new)?;
        if url.origin() != self.base.origin()
            || url.username() != self.base.username()
            || url.password() != self.base.password()
            || url.fragment().is_some()
        {
            return Err(Error::new(
                "adapter HTTP request must stay on its configured origin",
            ));
        }
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Delete => reqwest::Method::DELETE,
        };
        let mut builder = self
            .client
            .request(method, &request.url)
            .query(&request.query);
        if let Some(credential) = &self.credential {
            if url.origin() != credential.origin.origin() {
                return Err(Error::new("adapter credential origin mismatch"));
            }
            builder = builder.header(reqwest::header::AUTHORIZATION, credential.header.clone());
        }
        if let Some(body) = request.body {
            builder = builder.json(&body);
        }
        if let Some(ms) = request.timeout_ms {
            builder = builder.timeout(Duration::from_millis(ms.clamp(1, 6 * 60 * 60 * 1000)));
        }
        let response = builder.send().await?;
        if response.status().is_client_error() || response.status().is_server_error() {
            let status = response.status();
            let bytes = read_bounded(response, MAX_MESSAGE_BYTES).await?;
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let message = body["error"]["message"]
                .as_str()
                .or_else(|| body["error"].as_str())
                .unwrap_or("upstream request failed");
            return Err(redact_error(
                Error::new(format!("adapter HTTP {status}: {message}")),
                self.credential.as_ref(),
            ));
        }
        if response.status().is_redirection() {
            return Err(Error::new("adapter HTTP redirects are not allowed"));
        }
        Ok(response)
    }
    async fn execute(&self, request: AdapterRequest) -> Result<PluginOutput> {
        let mut session = self.plugin.session()?;
        let plan = self.prepare(&mut session, request)?;
        if plan.stream {
            return Err(Error::new("unexpected streaming HTTP plan"));
        }
        let responses = try_join_all(plan.requests.into_iter().map(|request| async move {
            let response = self.send(request).await?;
            // Lifecycle responses may be empty. The previous API only checked status.
            if matches!(plan.response, ResponseKind::Action) {
                return Ok(Value::Null);
            }
            let bytes = read_bounded(response, plan.response_limit).await?;
            Ok::<_, Error>(serde_json::from_slice(&bytes)?)
        }))
        .await?;
        session
            .invoke(&PluginInput::Decode {
                kind: plan.response,
                responses,
            })
            .map_err(|error| redact_error(error, self.credential.as_ref()))
    }
    pub async fn models(&self, timeout: Duration) -> Result<Vec<String>> {
        match self
            .execute(AdapterRequest::Models {
                timeout_ms: timeout.as_millis().min(u128::from(u64::MAX)) as u64,
            })
            .await?
        {
            PluginOutput::Models(models) => Ok(models),
            _ => Err(Error::new("adapter returned no model list")),
        }
    }
    pub async fn complete(&self, request: ChatRequest) -> Result<Completion> {
        match self.execute(AdapterRequest::Complete(request)).await? {
            PluginOutput::Completion(value) => Ok(value),
            _ => Err(Error::new("adapter returned no completion")),
        }
    }
    pub async fn model_state(&self) -> Result<ModelState> {
        match self.execute(AdapterRequest::State).await? {
            PluginOutput::State(value) => Ok(value),
            _ => Err(Error::new("adapter returned no model state")),
        }
    }
    pub async fn model_action(&self, action: ModelAction) -> Result<()> {
        match self.execute(AdapterRequest::Action(action)).await? {
            PluginOutput::Action => Ok(()),
            _ => Err(Error::new("adapter returned no action result")),
        }
    }
    pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>> {
        match self
            .execute(AdapterRequest::Search {
                query: query.into(),
            })
            .await?
        {
            PluginOutput::Search(value) => Ok(value),
            _ => Err(Error::new("adapter returned no search results")),
        }
    }
    pub async fn stream(&self, request: ChatRequest) -> Result<AdapterStream> {
        let mut session = self.plugin.session()?;
        let plan = self.prepare(&mut session, AdapterRequest::Stream(request))?;
        if !plan.stream || plan.requests.len() != 1 {
            return Err(Error::new("adapter returned no streaming HTTP plan"));
        }
        let response = self
            .send(plan.requests.into_iter().next().expect("one request"))
            .await?;
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if plan.stream_content_types.is_empty()
            || !plan
                .stream_content_types
                .iter()
                .any(|mime| content_type.starts_with(mime))
        {
            return Err(Error::new(format!(
                "backend does not support streaming (content-type: {content_type})"
            )));
        }
        Ok(AdapterStream {
            body: Box::pin(response.bytes_stream()),
            decoder: Decoder {
                session,
                buffer: Vec::new(),
            },
            ended: false,
            credential: self.credential.clone(),
        })
    }
}

fn validate_plan(base: &reqwest::Url, plan: &HttpPlan) -> Result<()> {
    if plan.requests.is_empty()
        || plan.requests.len() > 4
        || plan.response_limit == 0
        || plan.response_limit > MAX_MESSAGE_BYTES
    {
        return Err(Error::new("invalid adapter HTTP plan bounds"));
    }
    for request in &plan.requests {
        let url = reqwest::Url::parse(&request.url).map_err(Error::new)?;
        if url.origin() != base.origin()
            || url.username() != base.username()
            || url.password() != base.password()
            || url.fragment().is_some()
        {
            return Err(Error::new(
                "adapter HTTP request must stay on its configured origin",
            ));
        }
    }
    Ok(())
}
async fn read_bounded(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > limit {
            return Err(Error::new(format!(
                "adapter HTTP response exceeded {limit} bytes"
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

struct Decoder {
    session: Session,
    buffer: Vec<u8>,
}
impl Decoder {
    fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Option<StreamUpdate>> {
        if self.buffer.len() + bytes.len() > MAX_MESSAGE_BYTES {
            return Err(Error::new("adapter stream record exceeds 8 MiB"));
        }
        self.buffer.extend_from_slice(bytes);
        if eof && !self.buffer.is_empty() && self.buffer.last() != Some(&b'\n') {
            self.buffer.push(b'\n');
        }
        let Some(end) = self
            .buffer
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|end| end + 1)
        else {
            return Ok(None);
        };
        let records = std::str::from_utf8(&self.buffer[..end])?.to_owned();
        let output = self.session.invoke(&PluginInput::Stream { records })?;
        self.buffer.drain(..end);
        match output {
            PluginOutput::Stream(update) => Ok(Some(update)),
            _ => Err(Error::new("adapter returned no stream update")),
        }
    }
}

pub struct AdapterStream {
    body: Pin<Box<dyn Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send>>,
    decoder: Decoder,
    ended: bool,
    credential: Option<BearerCredential>,
}
impl AdapterStream {
    /// Dropping this stream cancels its upstream HTTP response. No worker or queue survives it.
    pub async fn next(&mut self) -> Result<Option<StreamUpdate>> {
        self.read_next()
            .await
            .map_err(|error| redact_error(error, self.credential.as_ref()))
    }
    async fn read_next(&mut self) -> Result<Option<StreamUpdate>> {
        if self.ended {
            return Ok(None);
        }
        loop {
            let chunk = self.body.next().await;
            let eof = chunk.is_none();
            let bytes = chunk.transpose()?.unwrap_or_default();
            if let Some(update) = self.decoder.push(&bytes, eof)? {
                self.ended = eof || update.done;
                return Ok(Some(update));
            }
            if eof {
                self.ended = true;
                return Ok(None);
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod freetoken_tests;
