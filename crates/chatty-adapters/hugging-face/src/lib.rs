//! Public Hugging Face GGUF catalogue translation, executed only inside WASM.
#[path = "../../shared/api.rs"]
pub mod adapter_api;
use adapter_api::*;
use serde_json::Value;

pub fn translate(input: PluginInput) -> PluginResult {
    match input {
        PluginInput::Prepare {
            base_url,
            request: AdapterRequest::Search { query },
        } => {
            let query = query.trim();
            if query.len() > 128 || query.chars().any(char::is_control) {
                return Err(
                    "model search must not exceed 128 bytes or contain control characters".into(),
                );
            }
            let mut request = HttpRequest::get(
                format!("{}/api/models", base_url.trim_end_matches('/')),
                20_000,
            );
            request.query = [
                ("search", query),
                ("filter", "gguf"),
                ("sort", "downloads"),
                ("direction", "-1"),
                ("limit", "20"),
                ("full", "true"),
            ]
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect();
            let mut plan = HttpPlan::new(vec![request], ResponseKind::Search);
            plan.response_limit = 2 * 1024 * 1024;
            Ok(PluginOutput::Plan(plan))
        }
        PluginInput::Decode {
            kind: ResponseKind::Search,
            responses,
        } => {
            let body = responses.first().ok_or("model search missing body")?;
            Ok(PluginOutput::Search(parse_model_search(
                &serde_json::to_vec(body).map_err(|e| e.to_string())?,
            )?))
        }
        _ => Err("Hugging Face catalogue supports model search only".into()),
    }
}
register_adapter!(translate);

pub fn parse_model_search(bytes: &[u8]) -> Result<Vec<SearchResult>, String> {
    let rows: Vec<Value> = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .take(20)
        .filter_map(|row| {
            if row["gated"].as_bool() == Some(true)
                || row["gated"].as_str().is_some()
                || row["private"].as_bool() == Some(true)
            {
                return None;
            }
            let id = row["id"].as_str()?;
            if id.len() > 180
                || id.split('/').count() != 2
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-._/".contains(c))
            {
                return None;
            }
            let mut files: Vec<String> = row["siblings"]
                .as_array()?
                .iter()
                .filter_map(|file| file["rfilename"].as_str())
                .filter(|name| {
                    name.ends_with(".gguf")
                        && !name.contains("-of-")
                        && name.len() + id.len() < 245
                        && !name.contains('/')
                        && !name.contains(':')
                        && !name.chars().any(char::is_control)
                })
                .take(128)
                .map(str::to_owned)
                .collect();
            files.sort();
            if files.is_empty() {
                return None;
            }
            Some(SearchResult {
                id: id.to_owned(),
                downloads: row["downloads"].as_u64().unwrap_or(0),
                files,
            })
        })
        .collect())
}

#[cfg(test)]
mod model_search_excludes_gated_split_and_unsafe_files {
    use super::*;
    use serde_json::json;
    #[test]
    fn model_search_excludes_gated_split_and_unsafe_files() {
        let payload = serde_json::to_vec(&json!([
            {"id":"owner/chat-GGUF", "downloads":42, "siblings":[
                {"rfilename":"chat-Q4_K_M.gguf"},
                {"rfilename":"chat-00001-of-00002.gguf"},
                {"rfilename":"nested/file.gguf"},
                {"rfilename":"README.md"}
            ]},
            {"id":"owner/gated", "gated":"auto", "siblings":[{"rfilename":"chat.gguf"}]},
            {"id":"../bad/repo", "siblings":[{"rfilename":"chat.gguf"}]}
        ]))
        .unwrap();
        let results = parse_model_search(&payload).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "owner/chat-GGUF");
        assert_eq!(results[0].downloads, 42);
        assert_eq!(results[0].files, ["chat-Q4_K_M.gguf"]);
        assert!(parse_model_search(b"not json").is_err());
    }
}

#[cfg(test)]
mod model_search_bounds_repositories_and_choices {
    use super::*;
    use serde_json::json;
    #[test]
    fn model_search_bounds_repositories_and_choices() {
        let files: Vec<_> = (0..200)
            .map(|i| json!({"rfilename":format!("model-{i}.gguf")}))
            .collect();
        let rows: Vec<_> = (0..30)
            .map(|i| json!({"id":format!("owner/model-{i}"), "siblings":files}))
            .collect();
        let results = parse_model_search(&serde_json::to_vec(&rows).unwrap()).unwrap();
        assert_eq!(results.len(), 20);
        assert!(results.iter().all(|r| r.files.len() == 128));
    }
}
