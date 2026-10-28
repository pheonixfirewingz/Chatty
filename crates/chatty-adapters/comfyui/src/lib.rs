//! ComfyUI text-to-image translation, executed only inside WASM.
//!
//! The built-in workflow is checkpoint loader, positive and negative CLIP prompts,
//! empty latent, KSampler, VAE decode, and a preview output. An administrator
//! can instead supply any API-format workflow with `{{placeholder}}` inputs.
#[path = "../../shared/api.rs"]
pub mod adapter_api;
use adapter_api::*;
use serde_json::{Map, Value, json};

const MAX_IMAGES: usize = 4;
const POLL_AFTER_MS: u64 = 1_000;

fn job_id(job: &str) -> Result<&str, String> {
    if job.is_empty()
        || job.len() > 64
        || !job.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err("ComfyUI job ID is invalid".into());
    }
    Ok(job)
}

/// Replace placeholders in every string of a template. A string that is exactly
/// one numeric placeholder becomes a JSON number, as ComfyUI node inputs expect.
fn fill(value: &mut Value, request: &ImageRequest, width: u32, height: u32) {
    match value {
        Value::String(text) => {
            let numeric = match text.as_str() {
                "{{seed}}" => Some(json!(request.seed)),
                "{{width}}" => Some(json!(width)),
                "{{height}}" => Some(json!(height)),
                "{{steps}}" => Some(json!(request.steps)),
                "{{guidance}}" => Some(json!(request.guidance)),
                _ => None,
            };
            if let Some(number) = numeric {
                *value = number;
                return;
            }
            if text.contains("{{") {
                *text = text
                    .replace("{{prompt}}", &request.prompt)
                    .replace("{{negative_prompt}}", &request.negative_prompt)
                    .replace("{{model}}", &request.model)
                    .replace("{{seed}}", &request.seed.to_string())
                    .replace("{{width}}", &width.to_string())
                    .replace("{{height}}", &height.to_string())
                    .replace("{{steps}}", &request.steps.to_string())
                    .replace("{{guidance}}", &request.guidance.to_string());
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| fill(item, request, width, height)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|item| fill(item, request, width, height)),
        _ => {}
    }
}

fn workflow(request: &ImageRequest) -> Result<Value, String> {
    // Latent dimensions must be multiples of eight.
    let width = (request.width / 8).max(8) * 8;
    let height = (request.height / 8).max(8) * 8;
    let model = request.model.trim();
    let uses_model = request
        .workflow
        .as_deref()
        .is_none_or(|template| template.contains("{{model}}"));
    if uses_model && (model.is_empty() || model.len() > 256 || model.chars().any(char::is_control))
    {
        return Err("image model name is invalid".into());
    }
    if let Some(template) = &request.workflow {
        let mut graph: Value = serde_json::from_str(template)
            .map_err(|error| format!("image workflow is not valid JSON: {error}"))?;
        if !graph.is_object() {
            return Err("image workflow must be an API-format JSON object".into());
        }
        fill(&mut graph, request, width, height);
        return Ok(graph);
    }
    Ok(json!({
        "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": model}},
        "2": {"class_type": "CLIPTextEncode", "inputs": {"text": request.prompt, "clip": ["1", 1]}},
        "3": {"class_type": "CLIPTextEncode", "inputs": {"text": request.negative_prompt, "clip": ["1", 1]}},
        "4": {"class_type": "EmptyLatentImage", "inputs": {"width": width, "height": height, "batch_size": 1}},
        "5": {"class_type": "KSampler", "inputs": {
            "model": ["1", 0], "positive": ["2", 0], "negative": ["3", 0], "latent_image": ["4", 0],
            "seed": request.seed, "steps": request.steps, "cfg": request.guidance,
            "sampler_name": "euler", "scheduler": "normal", "denoise": 1.0
        }},
        "6": {"class_type": "VAEDecode", "inputs": {"samples": ["5", 0], "vae": ["1", 2]}},
        // Preview output lives in ComfyUI's temporary directory, not its gallery.
        "7": {"class_type": "PreviewImage", "inputs": {"images": ["6", 0]}}
    }))
}

fn execution_error(status: &Value) -> String {
    status["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|message| message[0] == "execution_error")
        .and_then(|message| message[1]["exception_message"].as_str())
        .map(|message| format!("ComfyUI execution failed: {}", message.trim()))
        .unwrap_or_else(|| "ComfyUI execution failed".into())
}

fn history(body: &Value) -> Result<ImageJob, String> {
    let Some(entry) = body.as_object().and_then(|entries| entries.values().next()) else {
        // Unknown until the queued job starts; the host keeps its current job ID.
        return Ok(ImageJob::Pending {
            job: String::new(),
            poll_after_ms: POLL_AFTER_MS,
        });
    };
    let status = &entry["status"];
    if status["status_str"] == "error" {
        return Err(execution_error(status));
    }
    let empty = Map::new();
    let images: Vec<_> = entry["outputs"]
        .as_object()
        .unwrap_or(&empty)
        .values()
        .filter_map(|output| output["images"].as_array())
        .flatten()
        .filter_map(|image| {
            Some(ImageOutput::Get {
                path: "view".into(),
                query: vec![
                    ("filename".into(), image["filename"].as_str()?.into()),
                    (
                        "subfolder".into(),
                        image["subfolder"].as_str().unwrap_or("").into(),
                    ),
                    (
                        "type".into(),
                        image["type"].as_str().unwrap_or("output").into(),
                    ),
                ],
            })
        })
        .take(MAX_IMAGES)
        .collect();
    if !images.is_empty() {
        return Ok(ImageJob::Ready { images });
    }
    if status["completed"] == true {
        return Err("ComfyUI finished without producing an image".into());
    }
    Ok(ImageJob::Pending {
        job: String::new(),
        poll_after_ms: POLL_AFTER_MS,
    })
}

pub fn translate(input: PluginInput) -> PluginResult {
    match input {
        PluginInput::Prepare { base_url, request } => {
            let base = base_url.trim_end_matches('/');
            Ok(PluginOutput::Plan(match request {
                // Custom workflows may load split diffusion models instead of checkpoints.
                AdapterRequest::Models { timeout_ms } => HttpPlan::new(
                    vec![
                        HttpRequest::get(format!("{base}/models/checkpoints"), timeout_ms),
                        HttpRequest::get(format!("{base}/models/diffusion_models"), timeout_ms),
                    ],
                    ResponseKind::Models,
                ),
                AdapterRequest::Image(request) => HttpPlan::new(
                    vec![HttpRequest::post(
                        format!("{base}/prompt"),
                        json!({"prompt": workflow(&request)?, "client_id": "chatty"}),
                        Some(request.timeout_ms.min(60_000)),
                    )],
                    ResponseKind::Image,
                ),
                AdapterRequest::ImageStatus { job } => HttpPlan::new(
                    vec![HttpRequest::get(
                        format!("{base}/history/{}", job_id(&job)?),
                        20_000,
                    )],
                    ResponseKind::Image,
                ),
                AdapterRequest::ImageCancel { job } => {
                    let job = job_id(&job)?;
                    // Remove a queued job, and stop it only if it is the one running.
                    HttpPlan::new(
                        vec![
                            HttpRequest::post(
                                format!("{base}/queue"),
                                json!({"delete": [job]}),
                                Some(10_000),
                            ),
                            HttpRequest::post(
                                format!("{base}/interrupt"),
                                json!({"prompt_id": job}),
                                Some(10_000),
                            ),
                        ],
                        ResponseKind::Action,
                    )
                }
                AdapterRequest::ImageUnload => HttpPlan::new(
                    vec![HttpRequest::post(
                        format!("{base}/free"),
                        json!({"unload_models": true, "free_memory": true}),
                        Some(20_000),
                    )],
                    ResponseKind::Action,
                ),
                _ => return Err("ComfyUI adapter supports image generation only".into()),
            }))
        }
        PluginInput::Decode { kind, responses } => {
            let body = responses.first().ok_or("adapter response missing body")?;
            match kind {
                ResponseKind::Models => {
                    let mut models = Vec::new();
                    for list in &responses {
                        let list = list
                            .as_array()
                            .ok_or("ComfyUI model list is not an array")?;
                        for name in list.iter().filter_map(Value::as_str) {
                            if !models.iter().any(|known| known == name) {
                                models.push(name.to_owned());
                            }
                        }
                    }
                    Ok(PluginOutput::Models(models))
                }
                ResponseKind::Action => Ok(PluginOutput::Action),
                ResponseKind::Image => {
                    if let Some(job) = body["prompt_id"].as_str() {
                        if body["node_errors"]
                            .as_object()
                            .is_some_and(|e| !e.is_empty())
                        {
                            return Err("ComfyUI rejected the image workflow".into());
                        }
                        return Ok(PluginOutput::Image(ImageJob::Pending {
                            job: job_id(job)?.into(),
                            poll_after_ms: POLL_AFTER_MS,
                        }));
                    }
                    history(body).map(PluginOutput::Image)
                }
                _ => Err("unsupported ComfyUI response".into()),
            }
        }
        PluginInput::Stream { .. } => Err("ComfyUI adapter does not stream".into()),
    }
}
register_adapter!(translate);

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(body: Value) -> PluginResult {
        translate(PluginInput::Decode {
            kind: ResponseKind::Image,
            responses: vec![body],
        })
    }

    #[test]
    fn submit_builds_checkpoint_workflow() {
        let output = translate(PluginInput::Prepare {
            base_url: "http://comfy.example:8188/".into(),
            request: AdapterRequest::Image(ImageRequest {
                model: "sdxl.safetensors".into(),
                prompt: "a lighthouse".into(),
                negative_prompt: "blurry".into(),
                width: 1001,
                height: 512,
                steps: 20,
                guidance: 6.5,
                seed: u64::MAX,
                timeout_ms: 600_000,
                workflow: None,
            }),
        })
        .unwrap();
        let PluginOutput::Plan(plan) = output else {
            panic!("expected plan")
        };
        assert_eq!(plan.response, ResponseKind::Image);
        let request = &plan.requests[0];
        assert_eq!(request.url, "http://comfy.example:8188/prompt");
        assert_eq!(request.timeout_ms, Some(60_000));
        let graph = &request.body.as_ref().unwrap()["prompt"];
        assert_eq!(graph["1"]["inputs"]["ckpt_name"], "sdxl.safetensors");
        assert_eq!(graph["4"]["inputs"]["width"], 1000);
        assert_eq!(graph["5"]["inputs"]["seed"], u64::MAX);
        assert_eq!(graph["3"]["inputs"]["text"], "blurry");
    }

    #[test]
    fn decodes_job_lifecycle() {
        let Ok(PluginOutput::Image(ImageJob::Pending { job, .. })) =
            decode(json!({"prompt_id": "0f1e-22", "number": 3, "node_errors": {}}))
        else {
            panic!("expected pending submit")
        };
        assert_eq!(job, "0f1e-22");
        assert!(matches!(
            decode(json!({})),
            Ok(PluginOutput::Image(ImageJob::Pending { job, .. })) if job.is_empty()
        ));
        let Ok(PluginOutput::Image(ImageJob::Ready { images })) = decode(json!({"0f1e-22": {
            "status": {"status_str": "success", "completed": true},
            "outputs": {"7": {"images": [{"filename": "a.png", "subfolder": "", "type": "temp"}]}}
        }})) else {
            panic!("expected ready images")
        };
        let ImageOutput::Get { path, query } = &images[0] else {
            panic!("expected fetched output")
        };
        assert_eq!(path, "view");
        assert!(query.contains(&("type".into(), "temp".into())));
        let error = decode(json!({"0f1e-22": {
            "status": {"status_str": "error", "completed": false,
                "messages": [["execution_error", {"exception_message": "out of memory"}]]},
            "outputs": {}
        }}))
        .unwrap_err();
        assert!(error.contains("out of memory"));
    }

    fn submitted_graph(request: ImageRequest) -> Result<Value, String> {
        match translate(PluginInput::Prepare {
            base_url: "http://comfy.example".into(),
            request: AdapterRequest::Image(request),
        })? {
            PluginOutput::Plan(plan) => {
                Ok(plan.requests[0].body.as_ref().unwrap()["prompt"].clone())
            }
            _ => panic!("expected plan"),
        }
    }

    fn template_request(workflow: &str, model: &str) -> ImageRequest {
        ImageRequest {
            model: model.into(),
            prompt: "a \"quoted\" harbour".into(),
            negative_prompt: "blurry".into(),
            width: 1023,
            height: 768,
            steps: 4,
            guidance: 3.5,
            seed: 42,
            timeout_ms: 60_000,
            workflow: Some(workflow.into()),
        }
    }

    #[test]
    fn templates_fill_typed_and_embedded_placeholders() {
        let template = r#"{
            "1": {"class_type": "UNETLoader", "inputs": {"unet_name": "{{model}}", "weight_dtype": "default"}},
            "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "masterpiece, {{prompt}}"}},
            "3": {"class_type": "EmptySD3LatentImage", "inputs": {"width": "{{width}}", "height": "{{height}}", "batch_size": 1}},
            "4": {"class_type": "KSampler", "inputs": {"seed": "{{seed}}", "steps": "{{steps}}", "cfg": "{{guidance}}", "denoise": 1.0}},
            "5": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{negative_prompt}}"}},
            "6": {"class_type": "SaveImage", "inputs": {"filename_prefix": "chatty-{{seed}}"}}
        }"#;
        let graph = submitted_graph(template_request(template, "flux1-dev.safetensors")).unwrap();
        assert_eq!(graph["1"]["inputs"]["unet_name"], "flux1-dev.safetensors");
        assert_eq!(
            graph["2"]["inputs"]["text"],
            "masterpiece, a \"quoted\" harbour"
        );
        assert_eq!(
            graph["3"]["inputs"]["width"], 1016,
            "latent sides round to multiples of 8"
        );
        assert_eq!(graph["4"]["inputs"]["seed"], 42);
        assert_eq!(graph["4"]["inputs"]["steps"], 4);
        assert_eq!(graph["4"]["inputs"]["cfg"], 3.5);
        assert_eq!(graph["5"]["inputs"]["text"], "blurry");
        assert_eq!(graph["6"]["inputs"]["filename_prefix"], "chatty-42");
    }

    #[test]
    fn templates_without_a_model_placeholder_need_no_model() {
        let template =
            r#"{"1": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}"}}}"#;
        assert!(submitted_graph(template_request(template, "")).is_ok());
        let with_model = r#"{"1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "{{model}}"}}}"#;
        assert!(submitted_graph(template_request(with_model, "")).is_err());
        assert!(submitted_graph(template_request("[1, 2]", "m")).is_err());
    }

    #[test]
    fn model_list_merges_checkpoints_and_diffusion_models() {
        let Ok(PluginOutput::Models(models)) = translate(PluginInput::Decode {
            kind: ResponseKind::Models,
            responses: vec![
                json!(["sdxl.safetensors", "shared.safetensors"]),
                json!(["flux1-dev.safetensors", "shared.safetensors"]),
            ],
        }) else {
            panic!("expected models")
        };
        assert_eq!(
            models,
            [
                "sdxl.safetensors",
                "shared.safetensors",
                "flux1-dev.safetensors"
            ]
        );
    }

    #[test]
    fn rejects_job_ids_that_change_the_path() {
        for job in ["", "../queue", "a/b", "a?b"] {
            assert!(
                translate(PluginInput::Prepare {
                    base_url: "http://comfy.example".into(),
                    request: AdapterRequest::ImageStatus { job: job.into() },
                })
                .is_err()
            );
        }
    }
}
