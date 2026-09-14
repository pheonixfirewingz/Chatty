use super::*;

pub(super) fn tls_config(cert: &str, key: &str) -> Result<ServerConfig> {
    let certs: Vec<CertificateDer<'static>> =
        pemfile::certs(&mut BufReader::new(File::open(cert)?))
            .collect::<std::result::Result<_, _>>()?;
    let key: PrivateKeyDer<'static> = pemfile::private_key(&mut BufReader::new(File::open(key)?))?
        .context("missing private key")?;
    Ok(
        ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_no_client_auth()
            .with_single_cert(certs, key)?,
    )
}

pub(super) async fn serve(
    stream: tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    app: App,
) -> Result<()> {
    ACTIVE_CONNECTIONS.fetch_add(1, Ordering::Relaxed);
    let connection_id = new_uuid();
    let identity = Arc::new(RwLock::new(None::<String>));
    let (mut rd, mut wr) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::channel::<Out>(32); // bounded queue is transport backpressure
    let mut codec = ProtocolCodec::new()?;
    // Millis of the last byte activity in either direction; drives idle close.
    let last_activity = Arc::new(AtomicU64::new(unix_ms()));
    let busy = Arc::new(AtomicU64::new(0));
    let writer = {
        let last_activity = last_activity.clone();
        tokio::spawn(async move {
            while let Some((ty, id, payload)) = rx.recv().await {
                codec.write_payload(&mut wr, ty, id, &payload).await?;
                last_activity.store(unix_ms(), Ordering::Relaxed);
            }
            Ok::<_, ProtocolError>(())
        })
    };
    // The sole JSON use is the version handshake.
    tx.send((
        MessageType::Handshake,
        0,
        serde_json::to_vec(
            &json!({"protocol":10,"encoding":"bincode2","compression":"zstd","tls":"1.3"}),
        )?
        .into(),
    ))
    .await?;
    let mut published = app.deltas.subscribe();
    let forward_tx = tx.clone();
    let forward_identity = identity.clone();
    let (lag_tx, mut lag_rx) = watch::channel(false);
    let forwarder_connection_id = connection_id.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            match published.recv().await {
                Ok(event) if event.origin != forwarder_connection_id => {
                    if delta_visible(
                        forward_identity.read().await.as_deref(),
                        &forwarder_connection_id,
                        &event,
                    ) && forward_tx
                        .send((MessageType::Delta, 0, event.encoded.clone()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Never leave a connected client with a silent revision gap.
                    // Closing makes the client reconnect and Resume from its last
                    // applied revision through the authoritative delta log.
                    let _ = lag_tx.send(true);
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
        Ok::<_, ProtocolError>(())
    });
    let idle = tokio::time::sleep(IDLE_CLOSE);
    tokio::pin!(idle);
    loop {
        let incoming = tokio::select! {
            frame = read_frame(&mut rd) => {
                last_activity.store(unix_ms(), Ordering::Relaxed);
                idle.as_mut().reset(tokio::time::Instant::now() + IDLE_CLOSE);
                frame
            }
            changed = lag_rx.changed() => {
                if changed.is_ok() && *lag_rx.borrow() {
                    break;
                }
                continue;
            }
            _ = &mut idle => {
                let recent = unix_ms().saturating_sub(last_activity.load(Ordering::Relaxed))
                    < IDLE_CLOSE.as_millis() as u64;
                if recent || busy.load(Ordering::Relaxed) > 0 {
                    // Outbound traffic or an in-flight generation (a slow
                    // model load can silence the wire for minutes) keeps the
                    // connection warm; wait out the remaining quiet period.
                    idle.as_mut().reset(tokio::time::Instant::now() + IDLE_CLOSE);
                    continue;
                }
                break;
            }
        };
        let frame = match incoming {
            Ok(f) => f,
            Err(ProtocolError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        };
        match frame.message_type {
            MessageType::Request => {
                let req: Request = decode(&frame.payload)?;
                let app = app.clone();
                let error_log = app.recent_errors.clone();
                let tx = tx.clone();
                let identity = identity.clone();
                let busy = busy.clone();
                let connection_id = connection_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = dispatch(
                        app,
                        tx.clone(),
                        identity,
                        connection_id,
                        frame.request_id,
                        req,
                        busy,
                    )
                    .await
                    {
                        let w = classify_error(&e);
                        let mut errors = error_log.lock().await;
                        errors.push(format!("{} · {e}", current_utc_timestamp()));
                        if errors.len() > 20 {
                            errors.remove(0);
                        }
                        let _ = tx
                            .send((
                                MessageType::Error,
                                frame.request_id,
                                encode(&w).unwrap_or_default().into(),
                            ))
                            .await;
                    }
                });
            }
            MessageType::Cancel => {
                if let Some(cancel) = app
                    .cancellations
                    .lock()
                    .await
                    .remove(&(connection_id.clone(), frame.request_id))
                {
                    let _ = cancel.send(true);
                }
            }
            _ => return Err(ProtocolError::Invalid("unexpected client message").into()),
        }
    }
    forwarder.abort();
    drop(tx);
    cancel_connection(&app.cancellations, connection_id).await;
    writer.await??;
    ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
    Ok(())
}

pub(super) async fn cancel_connection(registry: &CancellationRegistry, connection_id: String) {
    let mut cancellations = registry.lock().await;
    cancellations.retain(|(id, _), cancel| {
        if *id == connection_id {
            let _ = cancel.send(true);
            false
        } else {
            true
        }
    });
}
