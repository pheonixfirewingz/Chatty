//! Native Ollama HTTP and NDJSON translation, executed only inside WASM.
#[path = "../../shared/api.rs"]
pub mod adapter_api;
use adapter_api::*;
use serde_json::{Value, json};

fn base_url(url: &str) -> &str {
    let url = url.trim_end_matches('/');
    url.strip_suffix("/v1").unwrap_or(url)
}
fn model_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err("model name is invalid".into());
    }
    Ok(name)
}
fn chat(base: &str, request: ChatRequest, stream: bool) -> HttpPlan {
    let options = request.options;
    let mut body =
        json!({"model":request.model,"messages":request.messages,"stream":stream,"options":{}});
    if let Some(value) = options.keep_alive {
        body["keep_alive"] = json!(value);
    }
    if let Some(value) = options.temperature {
        body["options"]["temperature"] = json!(value);
    }
    if let Some(value) = options.top_p {
        body["options"]["top_p"] = json!(value);
    }
    if let Some(value) = options.top_k {
        body["options"]["top_k"] = json!(value);
    }
    if let Some(value) = options.context_tokens {
        body["options"]["num_ctx"] = json!(value);
    }
    if let Some(value) = options.max_tokens {
        body["options"]["num_predict"] = json!(value);
    }
    if let Some(value) = options.repeat_penalty {
        body["options"]["repeat_penalty"] = json!(value);
    }
    if let Some(value) = options.seed {
        body["options"]["seed"] = json!(value);
    }
    if options.disable_reasoning {
        body["think"] = json!(false);
    }
    let mut plan = HttpPlan::new(
        vec![HttpRequest::post(
            format!("{base}/api/chat"),
            body,
            request.timeout_ms,
        )],
        ResponseKind::Completion,
    );
    plan.stream = stream;
    if stream {
        plan.stream_content_types = vec!["application/x-ndjson".into(), "application/json".into()];
    }
    plan
}

pub fn translate(input: PluginInput) -> PluginResult {
    Ok(match input {
        PluginInput::Prepare {
            base_url: url,
            request,
        } => {
            let base = base_url(&url);
            PluginOutput::Plan(match request {
                AdapterRequest::Complete(request) => chat(base, request, false),
                AdapterRequest::Stream(request) => chat(base, request, true),
                AdapterRequest::Models { timeout_ms } => HttpPlan::new(
                    vec![HttpRequest::get(format!("{base}/api/tags"), timeout_ms)],
                    ResponseKind::Models,
                ),
                AdapterRequest::Pull { model } => {
                    let mut plan = HttpPlan::new(
                        vec![HttpRequest::post(
                            format!("{base}/api/pull"),
                            json!({"model": model_name(&model)?, "stream": true}),
                            Some(6 * 60 * 60 * 1000),
                        )],
                        ResponseKind::Action,
                    );
                    plan.stream = true;
                    plan.stream_content_types =
                        vec!["application/x-ndjson".into(), "application/json".into()];
                    plan
                }
                AdapterRequest::State => HttpPlan::new(
                    ["version", "tags", "ps"]
                        .iter()
                        .map(|path| HttpRequest::get(format!("{base}/api/{path}"), 20_000))
                        .collect(),
                    ResponseKind::State,
                ),
                AdapterRequest::Action(action) => {
                    let mut request = match action {
                        ModelAction::Pull { model } => HttpRequest::post(
                            format!("{base}/api/pull"),
                            json!({"model":model_name(&model)?,"stream":false}),
                            Some(6 * 60 * 60 * 1000),
                        ),
                        ModelAction::Delete { model } => {
                            let mut request = HttpRequest::post(
                                format!("{base}/api/delete"),
                                json!({"model":model_name(&model)?}),
                                None,
                            );
                            request.method = HttpMethod::Delete;
                            request
                        }
                        ModelAction::Load { model, keep_alive } => HttpRequest::post(
                            format!("{base}/api/generate"),
                            json!({"model":model_name(&model)?,"keep_alive":keep_alive,"stream":false}),
                            None,
                        ),
                        ModelAction::Unload { model } => HttpRequest::post(
                            format!("{base}/api/generate"),
                            json!({"model":model_name(&model)?,"keep_alive":0,"stream":false}),
                            None,
                        ),
                    };
                    request.timeout_ms = Some(request.timeout_ms.unwrap_or(6 * 60 * 60 * 1000));
                    HttpPlan::new(vec![request], ResponseKind::Action)
                }
                _ => return Err("Ollama adapter does not support catalogue search".into()),
            })
        }
        PluginInput::Decode { kind, responses } => {
            let body = responses.first().ok_or("adapter response missing body")?;
            match kind {
                ResponseKind::Models => PluginOutput::Models(
                    body["models"]
                        .as_array()
                        .ok_or("models response missing data")?
                        .iter()
                        .filter_map(|v| v["name"].as_str().map(str::to_owned))
                        .collect(),
                ),
                ResponseKind::Completion => PluginOutput::Completion(Completion {
                    content: body["message"]["content"].as_str().map(str::to_owned),
                    finish_reason: body["done_reason"].as_str().map(str::to_owned),
                    usage: TokenUsage {
                        prompt_tokens: body["prompt_eval_count"].as_u64().unwrap_or(0),
                        completion_tokens: body["eval_count"].as_u64().unwrap_or(0),
                    },
                }),
                ResponseKind::Action => PluginOutput::Action,
                ResponseKind::State => {
                    if responses.len() != 3 {
                        return Err(
                            "Ollama state requires version, tags, and running models".into()
                        );
                    }
                    let tags = &responses[1];
                    let running = &responses[2];
                    PluginOutput::State(ModelState {
                        version: body["version"].as_str().unwrap_or("unknown").into(),
                        models: tags["models"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|m| {
                                Some(InstalledModel {
                                    name: m["name"].as_str()?.into(),
                                    size: m["size"].as_u64().unwrap_or(0),
                                    modified_at: m["modified_at"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .into(),
                                    family: m["details"]["family"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .into(),
                                    parameter_size: m["details"]["parameter_size"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .into(),
                                    quantization_level: m["details"]["quantization_level"]
                                        .as_str()
                                        .unwrap_or_default()
                                        .into(),
                                })
                            })
                            .collect(),
                        running_models: running["models"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|m| {
                                Some(RunningModel {
                                    name: m["name"].as_str()?.into(),
                                    size: m["size"].as_u64().unwrap_or(0),
                                    size_vram: m["size_vram"].as_u64().unwrap_or(0),
                                    expires_at: m["expires_at"].as_str().unwrap_or_default().into(),
                                })
                            })
                            .collect(),
                    })
                }
                _ => return Err("unsupported Ollama response".into()),
            }
        }
        PluginInput::Stream { records } => {
            let mut update = StreamUpdate::default();
            for line in records
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
            {
                let value: Value = serde_json::from_str(line)
                    .map_err(|_| "malformed Ollama NDJSON stream event")?;
                if let Some(error) = value["error"].as_str() {
                    return Err(format!("Ollama operation failed: {error}"));
                }
                if let Some(status) = value["status"].as_str() {
                    update.download = Some(DownloadProgress {
                        status: status.chars().take(256).collect(),
                        digest: value["digest"]
                            .as_str()
                            .map(|s| s.chars().take(128).collect()),
                        total: value["total"].as_u64(),
                        completed: value["completed"].as_u64(),
                    });
                    if status == "success" {
                        update.done = true;
                        break;
                    }
                }
                if let Some(text) = value["message"]["content"].as_str() {
                    update.text.push_str(text);
                }
                update.prompt_tokens = value["prompt_eval_count"].as_u64().or(update.prompt_tokens);
                update.completion_tokens =
                    value["eval_count"].as_u64().or(update.completion_tokens);
                if value["done"].as_bool().unwrap_or(false) {
                    update.done = true;
                    break;
                }
            }
            PluginOutput::Stream(update)
        }
    })
}
register_adapter!(translate);
