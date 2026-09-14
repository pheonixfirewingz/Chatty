use super::*;

pub(super) async fn extract_memory(
    app: &App,
    user_id: &str,
    conversation_id: &str,
) -> Result<String> {
    let model = selected_model(app).await?;
    let recent: Vec<String> = sqlx::query_scalar(
        "SELECT author_type || ': ' || COALESCE(v.content,m.content) FROM messages m LEFT JOIN variants v ON v.id=m.selected_variant_id AND v.message_id=m.id WHERE m.conversation_id=? AND m.parent_id IS NULL ORDER BY m.revision DESC LIMIT 40",
    )
    .bind(conversation_id)
    .fetch_all(&app.db)
    .await?;
    if recent.is_empty() {
        bail!("conversation has no history to extract")
    }
    let response = app
        .http
        .post(format!("{}/chat/completions", adapter_url(app).await?))
        .timeout(Duration::from_secs(30))
        .json(&json!({
            "model": &model,
            "messages": [
                {"role":"system","content":"Extract exactly one durable roleplay fact worth remembering from the transcript. Return only the fact as one concise sentence. Do not add labels, markdown, instructions, guesses, or private reasoning. If there is no durable fact, return NONE."},
                {"role":"user","content":recent.into_iter().rev().collect::<Vec<_>>().join("\n")}
            ],
            "stream": false,
            "max_tokens": 128,
            "temperature": 0.1
        }))
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    record_token_usage(app, user_id, token_usage_from_openai(&response)).await?;
    validate_extracted_memory(
        response["choices"][0]["message"]["content"]
            .as_str()
            .context("memory extraction response missing content")?,
    )
}

pub(super) fn validate_extracted_memory(value: &str) -> Result<String> {
    let content = value.trim();
    if content.eq_ignore_ascii_case("none") || content.is_empty() {
        bail!("model found no durable memory")
    }
    if content.chars().count() > 1024 || content.contains("```\n") {
        bail!("model returned an invalid memory")
    }
    Ok(content.to_owned())
}

pub(super) async fn probe_backend(app: &App) -> Result<Vec<String>> {
    let v: Value = app
        .http
        .get(format!("{}/models", adapter_url(app).await?))
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(v["data"]
        .as_array()
        .context("models response missing data")?
        .iter()
        .filter_map(|x| x["id"].as_str().map(str::to_owned))
        .collect())
}

pub(super) async fn selected_model(app: &App) -> Result<String> {
    let config = load_broker_config(&app.db).await?;
    let models = probe_backend(app).await?;
    if config.model.is_empty() {
        return models.into_iter().next().context("no model loaded");
    }
    if models.iter().any(|model| model == &config.model) {
        Ok(config.model)
    } else {
        bail!("configured model '{}' is not installed", config.model)
    }
}

pub(super) async fn generate(app: &App, job: Generation<'_>) -> Result<()> {
    job.busy.fetch_add(1, Ordering::Relaxed);
    let _busy_guard = BusyGuard(job.busy.clone());
    let Generation {
        tx,
        request_id: req_id,
        user_id: uid,
        conversation_id: cid,
        speaker_id: speaker,
        parent_id: parent,
        mut cancel,
        origin,
        busy: _,
    } = job;
    let config = load_broker_config(&app.db).await?;
    let model = selected_model(app).await?;
    let participants=sqlx::query("SELECT c.id,c.name,c.system_prompt,c.description,c.personality,c.scenario,c.appearance,c.age,c.gender,c.race,c.misc,c.example_dialogue FROM participants p JOIN characters c ON c.id=p.character_id WHERE p.conversation_id=? ORDER BY p.position").bind(cid).fetch_all(&app.db).await?;
    let sid = select_speaker(app, uid, cid, speaker, &participants).await?;
    let character = participants
        .iter()
        .find(|r| r.get::<String, _>("id") == sid)
        .context("speaker is not a participant")?;
    let recent=sqlx::query("SELECT m.author_type,m.author_id,COALESCE(v.content,m.content) AS content FROM messages m LEFT JOIN variants v ON v.id=m.selected_variant_id AND v.message_id=m.id WHERE m.conversation_id=? AND m.parent_id IS NULL ORDER BY m.revision DESC LIMIT 80").bind(cid).fetch_all(&app.db).await?;
    let immediate = recent
        .iter()
        .take(6)
        .map(|r| clip(&r.get::<String, _>("content"), 4096))
        .collect::<Vec<_>>()
        .join("\n");
    let world_context = chatty_protocol::world::select_world_context(
        &load_worlds(&app.db, uid).await?,
        &sid,
        &immediate,
        8192,
    );
    let memories:Vec<String>=sqlx::query_scalar("SELECT content FROM memories WHERE owner_id=? AND (conversation_id IS NULL OR conversation_id=?) AND (character_id IS NULL OR character_id=?) LIMIT 64").bind(uid).bind(cid).bind(&sid).fetch_all(&app.db).await?;
    let context =
        sqlx::query("SELECT CAST(state AS TEXT) AS state,summary FROM conversations WHERE id=?")
            .bind(cid)
            .fetch_one(&app.db)
            .await?;
    let system = format!(
        "{}\nYou are {}.\nDescription: {}\nPersonality: {}\nAppearance: {}\nAge: {}\nGender: {}\nRace: {}\nMisc: {}\nScenario: {}\nExample dialogue:\n{}\nGroup participants:\n{}\nWorld state:\n{}\nStory summary:\n{}\nLore:\n{}\nMemory:\n{}",
        clip(&character.get::<String, _>("system_prompt"), 16_384),
        character.get::<String, _>("name"),
        clip(&character.get::<String, _>("description"), 16_384),
        clip(&character.get::<String, _>("personality"), 16_384),
        clip(&character.get::<String, _>("appearance"), 16_384),
        clip(&character.get::<String, _>("age"), 512),
        clip(&character.get::<String, _>("gender"), 512),
        clip(&character.get::<String, _>("race"), 512),
        clip(&character.get::<String, _>("misc"), 2_048),
        clip(&character.get::<String, _>("scenario"), 16_384),
        clip(&character.get::<String, _>("example_dialogue"), 16_384),
        participants
            .iter()
            .map(|r| format!(
                "{} — description: {}; personality: {}; appearance: {}; scenario: {}",
                r.get::<String, _>("name"),
                clip(&r.get::<String, _>("description"), 512),
                clip(&r.get::<String, _>("personality"), 512),
                clip(&r.get::<String, _>("appearance"), 512),
                clip(&r.get::<String, _>("scenario"), 512)
            ))
            .collect::<Vec<_>>()
            .join(", "),
        clip(&context.get::<String, _>("state"), 32_768),
        clip(&context.get::<String, _>("summary"), 32_768),
        world_context,
        memories
            .iter()
            .map(|memory| clip(memory, 4096))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let mut messages = vec![json!({"role":"system","content":system})];
    for r in recent.into_iter().rev() {
        let author_type = r.get::<String, _>("author_type");
        let role = match author_type.as_str() {
            "user" => "user",
            "system" => "system",
            _ => "assistant",
        };
        messages.push(json!({"role":role,"content":clip(&r.get::<String,_>("content"),8192)}));
    }
    let mid = new_uuid();
    tx.send((
        MessageType::Response,
        req_id,
        encode(&Response::GenerationStarted {
            message_id: mid.clone(),
            character_id: sid.clone(),
        })?
        .into(),
    ))
    .await?;
    let (request_url, mut request_body, native_ollama) = if config.use_ollama_api {
        (
            format!("{}/api/chat", ollama_base_url(&config.adapter_url)),
            json!({
                "model": &model,
                "messages": messages,
                "stream": true,
                "keep_alive": config.keep_alive,
                "options": {
                    "temperature": config.temperature,
                    "top_p": config.top_p,
                    "top_k": config.top_k,
                    "num_ctx": config.num_ctx,
                    "num_predict": config.num_predict,
                    "repeat_penalty": config.repeat_penalty,
                    "seed": config.seed
                }
            }),
            true,
        )
    } else {
        (
            format!("{}/chat/completions", config.adapter_url),
            json!({
                "model": &model,
                "messages": messages,
                "stream": true,
                "stream_options": {"include_usage": true},
                "temperature": config.temperature,
                "top_p": config.top_p,
                "seed": config.seed
            }),
            false,
        )
    };
    if !native_ollama && config.num_predict >= 0 {
        request_body["max_tokens"] = json!(config.num_predict);
    }
    let response_future = app.http.post(request_url).json(&request_body).send();
    let response=tokio::select!{biased;
        changed=cancel.changed()=>{
            if changed.is_err()||*cancel.borrow(){
                let revision:i64=sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM deltas WHERE owner_id=?").bind(uid).fetch_one(&app.db).await?;
                tx.send((MessageType::StreamEnd,req_id,encode(&Response::GenerationFinished{message_id:mid,revision,cancelled:true})?.into())).await?;
                return Ok(());
            }
            bail!("generation cancellation channel changed unexpectedly")
        }
        response=response_future=>response?
    }.error_for_status()?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let lower_content_type = content_type.to_ascii_lowercase();
    if native_ollama {
        if !lower_content_type.starts_with("application/x-ndjson")
            && !lower_content_type.starts_with("application/json")
        {
            bail!("Ollama does not support streaming NDJSON (content-type: {content_type})")
        }
    } else if !lower_content_type.starts_with("text/event-stream") {
        bail!("backend does not support streaming SSE (content-type: {content_type})")
    }
    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut complete = String::new();
    let mut sse_buffer = Vec::new();
    let mut seq = 0;
    let mut deadline = Instant::now() + Duration::from_millis(60);
    let mut cancelled = false;
    let mut done = false;
    let mut usage = TokenUsage::default();
    loop {
        enum Event<T> {
            Cancel,
            Timeout,
            Data(Option<T>),
        }
        let event = tokio::select! {
            biased;
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() { Event::Cancel } else { continue }
            }
            _ = tokio::time::sleep_until(deadline) => Event::Timeout,
            item = stream.next() => Event::Data(item),
        };
        match event {
            Event::Cancel => {
                cancelled = true;
                break;
            }
            Event::Timeout => {}
            Event::Data(Some(chunk)) => {
                sse_buffer.extend_from_slice(&chunk?);
                done = if native_ollama {
                    drain_ollama_stream(&mut sse_buffer, &mut pending, &mut complete, &mut usage)?
                } else {
                    drain_sse(&mut sse_buffer, &mut pending, &mut complete, &mut usage)?
                };
            }
            Event::Data(None) => {
                if !sse_buffer.is_empty() {
                    sse_buffer.push(b'\n');
                    let _ = if native_ollama {
                        drain_ollama_stream(
                            &mut sse_buffer,
                            &mut pending,
                            &mut complete,
                            &mut usage,
                        )?
                    } else {
                        drain_sse(&mut sse_buffer, &mut pending, &mut complete, &mut usage)?
                    };
                }
                break;
            }
        }
        if !pending.is_empty()
            && (pending.split_whitespace().count() >= 32 || Instant::now() >= deadline || done)
        {
            seq += 1;
            let out = StreamChunk {
                message_id: mid.clone(),
                sequence: seq,
                text: std::mem::take(&mut pending),
            };
            tx.send((MessageType::StreamChunk, req_id, encode(&out)?.into()))
                .await?;
            deadline = Instant::now() + Duration::from_millis(60);
        } else if Instant::now() >= deadline {
            deadline = Instant::now() + Duration::from_millis(60);
        }
        if done {
            break;
        }
    }
    if !pending.is_empty() {
        seq += 1;
        tx.send((
            MessageType::StreamChunk,
            req_id,
            encode(&StreamChunk {
                message_id: mid.clone(),
                sequence: seq,
                text: pending,
            })?
            .into(),
        ))
        .await?;
    }
    record_token_usage(app, uid, usage).await?;
    let delta = persist_generation(app, uid, cid, &sid, &mid, &complete, parent.as_deref()).await?;
    let rev = delta.revision;
    let encoded: Bytes = encode(&delta)?.into();
    tx.send((MessageType::Delta, req_id, encoded.clone()))
        .await?;
    let _ = app.deltas.send(PublishedDelta {
        owner_id: uid.into(),
        origin,
        encoded,
    });
    tx.send((
        MessageType::StreamEnd,
        req_id,
        encode(&Response::GenerationFinished {
            message_id: mid,
            revision: rev,
            cancelled,
        })?
        .into(),
    ))
    .await?;
    Ok(())
}

pub(super) async fn persist_generation(
    app: &App,
    user_id: &str,
    conversation_id: &str,
    speaker_id: &str,
    generation_id: &str,
    content: &str,
    parent_id: Option<&str>,
) -> Result<StateDelta> {
    let is_variant = parent_id.is_some();
    let changed = if let Some(parent_id) = parent_id {
        encode(&DeltaPayload::Variant {
            message_id: parent_id.into(),
            content: content.into(),
        })?
    } else {
        encode(&DeltaPayload::Message {
            conversation_id: conversation_id.into(),
            author_type: "character".into(),
            author_id: Some(speaker_id.into()),
            content: content.into(),
            parent_id: None,
            selected_variant_id: None,
        })?
    };
    let mut transaction = app.db.begin().await?;
    let revision = delta_tx(
        &mut transaction,
        user_id,
        if is_variant { "variant" } else { "message" },
        generation_id,
        DeltaOperation::Add,
        &changed,
    )
    .await?;
    if let Some(parent_id) = parent_id {
        sqlx::query("INSERT INTO variants(id,message_id,content,revision) VALUES(?,?,?,?)")
            .bind(generation_id)
            .bind(parent_id)
            .bind(content)
            .bind(revision)
            .execute(&mut *transaction)
            .await?;
        let updated = sqlx::query(
            "UPDATE messages SET selected_variant_id=?,revision=? WHERE id=? AND conversation_id=?",
        )
        .bind(generation_id)
        .bind(revision)
        .bind(parent_id)
        .bind(conversation_id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            bail!("variant parent disappeared during generation")
        }
    } else {
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,parent_id,revision) VALUES(?,?,?,?,?,NULL,?)")
            .bind(generation_id)
            .bind(conversation_id)
            .bind("character")
            .bind(speaker_id)
            .bind(content)
            .bind(revision)
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await?;
    Ok(StateDelta {
        revision,
        entity_type: if is_variant {
            "variant".into()
        } else {
            "message".into()
        },
        entity_id: generation_id.into(),
        operation: DeltaOperation::Add,
        changed_fields: changed,
    })
}

pub(super) async fn select_speaker(
    app: &App,
    user_id: &str,
    cid: &str,
    explicit: Option<String>,
    participants: &[sqlx::sqlite::SqliteRow],
) -> Result<String> {
    if let Some(id) = explicit {
        return Ok(id);
    }
    if participants.is_empty() {
        bail!("conversation has no character")
    }
    let row = sqlx::query("SELECT kind,turn_index FROM conversations WHERE id=?")
        .bind(cid)
        .fetch_one(&app.db)
        .await?;
    let kind: i32 = row.get("kind");
    let turn: i64 = row.get("turn_index");
    let fallback = participants[turn as usize % participants.len()].get::<String, _>("id");
    if kind == ConversationKind::GroupManual as i32 {
        bail!("manual group mode requires an explicit speaker")
    }
    let selected = if kind == ConversationKind::GroupAutomatic as i32 {
        let names = participants
            .iter()
            .map(|r| r.get::<String, _>("name"))
            .collect::<Vec<_>>();
        let recent:Vec<String>=sqlx::query_scalar("SELECT content FROM messages WHERE conversation_id=? AND parent_id IS NULL ORDER BY revision DESC LIMIT 12").bind(cid).fetch_all(&app.db).await.unwrap_or_default();
        let choice = async {
            let model = selected_model(app).await.ok()?;
            let response=app.http.post(format!("{}/chat/completions",adapter_url(app).await.ok()?)).timeout(Duration::from_secs(15)).json(&json!({"model":model,"messages":[{"role":"system","content":"Choose exactly one next speaker name from the supplied list based on the recent roleplay. Output only the name."},{"role":"user","content":format!("Speakers: {}\nRecent roleplay:\n{}",names.join(", "),recent.into_iter().rev().collect::<Vec<_>>().join("\n"))}],"stream":false,"max_tokens":16})).send().await.ok()?.error_for_status().ok()?.json::<Value>().await.ok()?;
            record_token_usage(app, user_id, token_usage_from_openai(&response)).await.ok()?;
            let answer = response["choices"][0]["message"]["content"].as_str()?.trim();
            participants.iter().find(|r|r.get::<String,_>("name").eq_ignore_ascii_case(answer)).map(|r|r.get("id"))
        }.await;
        choice.unwrap_or(fallback)
    } else {
        fallback
    };
    sqlx::query("UPDATE conversations SET turn_index=turn_index+1 WHERE id=?")
        .bind(cid)
        .execute(&app.db)
        .await?;
    Ok(selected)
}
