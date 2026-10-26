//! FreeToken serving API translator. HTTP and credentials remain host-owned.
#[path = "../../shared/api.rs"]
pub mod adapter_api;
#[path = "../../shared/openai_compatible.rs"]
mod openai_compatible;
use adapter_api::*;
use serde_json::json;

pub fn translate(input: PluginInput) -> PluginResult {
    match input {
        PluginInput::Prepare { base_url, request } => {
            let base = base_url.trim_end_matches('/');
            let base = base.strip_suffix("/v1").unwrap_or(base);
            match request {
                AdapterRequest::Action(action) => {
                    let operation = match action {
                        ModelAction::Pull { .. } => "download",
                        ModelAction::Delete { .. } => "delete",
                        ModelAction::Load { .. } => "load",
                        ModelAction::Unload { .. } => "unload",
                    };
                    Err(format!(
                        "FreeToken {operation} unsupported on the serving API; manage models on the inference machine"
                    ))
                }
                AdapterRequest::Search { .. } => {
                    Err("FreeToken catalogue search unsupported".into())
                }
                AdapterRequest::State => Ok(PluginOutput::Plan(HttpPlan::new(
                    vec![HttpRequest::get(format!("{base}/health"), 20_000)],
                    ResponseKind::State,
                ))),
                request => {
                    let top_k = match &request {
                        AdapterRequest::Complete(chat) | AdapterRequest::Stream(chat) => {
                            chat.options.top_k
                        }
                        _ => None,
                    };
                    let mut output = openai_compatible::translate(PluginInput::Prepare {
                        base_url: format!("{base}/v1"),
                        request,
                    })?;
                    if let PluginOutput::Plan(plan) = &mut output {
                        for request in &mut plan.requests {
                            if let Some(body) =
                                request.body.as_mut().and_then(|body| body.as_object_mut())
                            {
                                // Accepted as extras upstream, but never used by its sampler.
                                body.remove("seed");
                                if let Some(value) = top_k {
                                    body.insert("top_k".into(), json!(value));
                                }
                            }
                        }
                    }
                    Ok(output)
                }
            }
        }
        PluginInput::Decode {
            kind: ResponseKind::State,
            responses,
        } => {
            let body = responses
                .first()
                .ok_or("FreeToken health response missing body")?;
            let status = body["status"]
                .as_str()
                .ok_or("FreeToken health response missing status")?;
            match status {
                "error" => {
                    return Err(format!(
                        "FreeToken health failed: {}",
                        body["message"].as_str().unwrap_or("unknown backend error")
                    ));
                }
                "ok" | "loading" => {}
                _ => return Err("FreeToken health response has unknown status".into()),
            }
            let mut state = ModelState {
                version: body["version"].as_str().unwrap_or("unknown").into(),
                ..ModelState::default()
            };
            if status == "ok" && body["maintenance"].as_str() == Some("serving") {
                let name = body["model"]
                    .as_str()
                    .ok_or("FreeToken health response missing model")?;
                state.running_models.push(RunningModel {
                    name: name.into(),
                    size: 0,
                    size_vram: 0,
                    expires_at: String::new(),
                });
            }
            // The serving API does not expose an inventory of installed weights.
            Ok(PluginOutput::State(state))
        }
        input => openai_compatible::translate(input),
    }
}

register_adapter!(translate);
