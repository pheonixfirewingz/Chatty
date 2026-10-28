//! ComfyUI text-to-image translation, executed only inside WASM.
//!
//! Jobs use a single-checkpoint workflow: checkpoint loader, positive and negative
//! CLIP prompts, empty latent, KSampler, VAE decode, and a preview output.
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

fn workflow(request: &ImageRequest) -> Result<Value, String> {
    let model = request.model.trim();
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        return Err("image model name is invalid".into());
    }
    // Latent dimensions must be multiples of eight.
    let width = (request.width / 8).max(8) * 8;
    let height = (request.height / 8).max(8) * 8;
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
                AdapterRequest::Models { timeout_ms } => HttpPlan::new(
                    vec![HttpRequest::get(
                        format!("{base}/models/checkpoints"),
                        timeout_ms,
                    )],
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
                ResponseKind::Models => Ok(PluginOutput::Models(
                    body.as_array()
                        .ok_or("ComfyUI checkpoint list is not an array")?
                        .iter()
                        .filter_map(|name| name.as_str().map(str::to_owned))
                        .collect(),
                )),
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
