//! Inference model management: listing, actions, search, and downloads.
use super::*;

pub(crate) async fn load_ollama_state(app: &App) -> Result<OllamaState> {
    let config = load_broker_config(&app.db).await?;
    let state = app.adapters.management(&config)?.model_state().await?;
    Ok(OllamaState {
        version: state.version,
        models: state
            .models
            .into_iter()
            .map(|model| OllamaModel {
                name: model.name,
                size: model.size,
                modified_at: model.modified_at,
                family: model.family,
                parameter_size: model.parameter_size,
                quantization_level: model.quantization_level,
            })
            .collect(),
        running_models: state
            .running_models
            .into_iter()
            .map(|model| OllamaRunningModel {
                name: model.name,
                size: model.size,
                size_vram: model.size_vram,
                expires_at: model.expires_at,
            })
            .collect(),
    })
}

pub(crate) async fn run_ollama_action(app: &App, action: OllamaAction) -> Result<()> {
    let config = load_broker_config(&app.db).await?;
    let model = match &action {
        OllamaAction::Pull { model }
        | OllamaAction::Delete { model }
        | OllamaAction::Load { model }
        | OllamaAction::Unload { model } => model,
    };
    let _operation = app.adapters.reserve_model_operation(
        &config,
        model,
        matches!(&action, OllamaAction::Pull { .. }),
    )?;
    // Loading occupies GPU memory; pulls and deletes do not.
    let _workload = match &action {
        OllamaAction::Load { .. } => Some(app.adapters.text_workload(&config).await),
        _ => None,
    };
    let action = match action {
        OllamaAction::Pull { model } => crate::adapter_api::ModelAction::Pull { model },
        OllamaAction::Delete { model } => crate::adapter_api::ModelAction::Delete { model },
        OllamaAction::Load { model } => crate::adapter_api::ModelAction::Load {
            model,
            keep_alive: config.keep_alive.clone(),
        },
        OllamaAction::Unload { model } => crate::adapter_api::ModelAction::Unload { model },
    };
    Ok(app
        .adapters
        .management(&config)?
        .model_action(action)
        .await?)
}

pub(crate) async fn search_hugging_face_models(
    app: &App,
    query: &str,
) -> Result<Vec<ModelSearchResult>> {
    Ok(app
        .adapters
        .catalogue()?
        .search(query)
        .await?
        .into_iter()
        .map(|result| ModelSearchResult {
            id: result.id,
            downloads: result.downloads,
            files: result.files,
        })
        .collect())
}

/// Download progress stays scoped to this request and connection. Dropping the
/// operation drops the upstream body and releases its concurrency slot.
pub(crate) async fn download_model(
    app: &App,
    tx: &mpsc::Sender<Out>,
    request_id: u64,
    model: String,
    mut cancel: watch::Receiver<bool>,
) -> Result<()> {
    let work = async {
        let config = load_broker_config(&app.db).await?;
        let _operation = app
            .adapters
            .reserve_model_operation(&config, &model, true)?;
        let mut stream = app.adapters.management(&config)?.pull(model).await?;
        let mut last_sent = None::<Instant>;
        while let Some(update) = stream.next().await? {
            if let Some(progress) = update.download
                && (update.done
                    || last_sent.is_none_or(|last| last.elapsed() >= Duration::from_millis(250)))
            {
                let progress = chatty_protocol::admin::ModelDownloadProgress {
                    status: progress.status.chars().take(256).collect(),
                    digest: progress
                        .digest
                        .map(|digest| digest.chars().take(128).collect()),
                    total: progress.total,
                    completed: progress.completed,
                };
                tx.send((
                    MessageType::Response,
                    request_id,
                    encode(&Response::ModelDownloadProgress(progress))?.into(),
                ))
                .await?;
                last_sent = Some(Instant::now());
            }
            if update.done {
                return Ok::<_, Error>(());
            }
        }
        bail!("model download ended before reporting success")
    };
    let cancelled = tokio::select! {
        biased;
        _ = async {
            if !*cancel.borrow() { let _ = cancel.changed().await; }
        } => true,
        _ = tx.closed() => true,
        result = work => { result?; false },
    };
    tx.send((
        MessageType::Response,
        request_id,
        encode(&Response::ModelDownloadFinished { cancelled })?.into(),
    ))
    .await?;
    Ok(())
}
