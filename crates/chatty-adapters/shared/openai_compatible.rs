//! Shared OpenAI-compatible HTTP and SSE translation.
use crate::adapter_api::*;
use serde_json::{Value, json};

fn usage(value: &Value) -> TokenUsage {
    TokenUsage {
        prompt_tokens: value["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
        completion_tokens: value["usage"]["completion_tokens"].as_u64().unwrap_or(0),
    }
}

fn chat(base: &str, request: ChatRequest, stream: bool) -> HttpPlan {
    let options = request.options;
    let mut body = json!({"model":request.model,"messages":request.messages,"stream":stream});
    if stream {
        body["stream_options"] = json!({"include_usage":true});
    }
    if let Some(value) = options.temperature {
        body["temperature"] = json!(value);
    }
    if let Some(value) = options.top_p {
        body["top_p"] = json!(value);
    }
    if let Some(value) = options.seed {
        body["seed"] = json!(value);
    }
    if let Some(value) = options.max_tokens.filter(|v| *v >= 0) {
        body["max_tokens"] = json!(value);
    }
    if options.disable_reasoning {
        body["reasoning_effort"] = json!("none");
    }
    let mut plan = HttpPlan::new(
        vec![HttpRequest::post(
            format!("{}/chat/completions", base.trim_end_matches('/')),
            body,
            request.timeout_ms,
        )],
        ResponseKind::Completion,
    );
    plan.stream = stream;
    if stream {
        plan.stream_content_types.push("text/event-stream".into());
    }
    plan
}

fn upstream_error(value: &Value) -> Result<(), String> {
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let message = error["message"]
            .as_str()
            .or_else(|| error.as_str())
            .unwrap_or("unknown backend error");
        return Err(format!("model generation failed: {message}"));
    }
    Ok(())
}

pub fn translate(input: PluginInput) -> PluginResult {
    Ok(match input {
        PluginInput::Prepare { base_url, request } => PluginOutput::Plan(match request {
            AdapterRequest::Models { timeout_ms } => HttpPlan::new(
                vec![HttpRequest::get(
                    format!("{}/models", base_url.trim_end_matches('/')),
                    timeout_ms,
                )],
                ResponseKind::Models,
            ),
            AdapterRequest::Complete(request) => chat(&base_url, request, false),
            AdapterRequest::Stream(request) => chat(&base_url, request, true),
            _ => return Err(
                "OpenAI-compatible adapter does not support model management or catalogue search"
                    .into(),
            ),
        }),
        PluginInput::Decode { kind, responses } => {
            let body = responses.first().ok_or("adapter response missing body")?;
            upstream_error(body)?;
            match kind {
                ResponseKind::Models => PluginOutput::Models(
                    body["data"]
                        .as_array()
                        .ok_or("models response missing data")?
                        .iter()
                        .filter_map(|v| v["id"].as_str().map(str::to_owned))
                        .collect(),
                ),
                ResponseKind::Completion => PluginOutput::Completion(Completion {
                    content: body["choices"][0]["message"]["content"]
                        .as_str()
                        .map(str::to_owned),
                    finish_reason: body["choices"][0]["finish_reason"]
                        .as_str()
                        .map(str::to_owned),
                    usage: usage(body),
                }),
                _ => return Err("unsupported OpenAI-compatible response".into()),
            }
        }
        PluginInput::Stream { records } => {
            let mut update = StreamUpdate::default();
            for line in records.lines() {
                let Some(data) = line
                    .trim_end_matches('\r')
                    .strip_prefix("data:")
                    .map(str::trim_start)
                else {
                    continue;
                };
                if data == "[DONE]" {
                    update.done = true;
                    break;
                }
                let value: Value = serde_json::from_str(data)
                    .map_err(|_| "malformed OpenAI-compatible SSE event")?;
                upstream_error(&value)?;
                update.finish_reason = value["choices"][0]["finish_reason"]
                    .as_str()
                    .map(str::to_owned)
                    .or(update.finish_reason);
                update.prompt_tokens = value["usage"]["prompt_tokens"]
                    .as_u64()
                    .or(update.prompt_tokens);
                update.completion_tokens = value["usage"]["completion_tokens"]
                    .as_u64()
                    .or(update.completion_tokens);
                if let Some(text) = value["choices"][0]["delta"]["content"].as_str() {
                    update.text.push_str(text);
                }
            }
            PluginOutput::Stream(update)
        }
    })
}
