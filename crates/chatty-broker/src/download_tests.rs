//! End-to-end management tests against the real embedded Ollama WASM adapter.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

struct Pull {
    model: String,
    finish: oneshot::Sender<()>,
    closed: oneshot::Receiver<()>,
}

struct Inference {
    address: String,
    pulls: mpsc::Receiver<Pull>,
    deleted: mpsc::Receiver<String>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Inference {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Inference {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let (pull_tx, pulls) = mpsc::channel(8);
        let (delete_tx, deleted) = mpsc::channel(8);
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.unwrap();
                        let pull_tx = pull_tx.clone();
                        let delete_tx = delete_tx.clone();
                        connections.spawn(async move {
                            let mut socket = tokio::io::BufReader::new(socket);
                            let mut line = String::new();
                            socket.read_line(&mut line).await.unwrap();
                            let path = line.split_whitespace().nth(1).unwrap().to_owned();
                            let mut length = 0;
                            loop {
                                line.clear();
                                socket.read_line(&mut line).await.unwrap();
                                if line == "\r\n" { break; }
                                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                                    length = value.trim().parse::<usize>().unwrap();
                                }
                            }
                            let mut bytes = vec![0; length];
                            socket.read_exact(&mut bytes).await.unwrap();
                            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                            if path == "/api/pull" {
                                assert_eq!(body["stream"], true);
                                let model = body["model"].as_str().unwrap().to_owned();
                                socket.get_mut().write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n").await.unwrap();
                                if model == "broken" {
                                    socket.get_mut().write_all(b"{\"status\":\"pulling\",\"total\":100,\"completed\":25}\n").await.unwrap();
                                    return;
                                }
                                // Fragment a record across writes; the decoder must retain it.
                                socket.get_mut().write_all(b"{\"status\":\"pulling layer\",\"digest\":\"sha256:test\",\"total\":100,").await.unwrap();
                                socket.get_mut().write_all(b"\"completed\":25}\n").await.unwrap();
                                let (finish, done) = oneshot::channel();
                                let (closed_tx, closed) = oneshot::channel();
                                pull_tx.send(Pull { model, finish, closed }).await.unwrap();
                                tokio::select! {
                                    _ = done => {
                                        let _ = socket.get_mut().write_all(b"{\"status\":\"success\"}\n").await;
                                    }
                                    _ = socket.read_u8() => {}
                                }
                                let _ = closed_tx.send(());
                            } else {
                                if path == "/api/delete" {
                                    delete_tx.send(body["model"].as_str().unwrap().to_owned()).await.unwrap();
                                }
                                let body = match path.as_str() {
                                    "/api/version" => "{\"version\":\"test\"}",
                                    "/api/tags" | "/api/ps" => "{\"models\":[]}",
                                    "/api/delete" => "{}",
                                    _ => panic!("unexpected path {path}"),
                                };
                                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                                socket.get_mut().write_all(response.as_bytes()).await.unwrap();
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            address,
            pulls,
            deleted,
            task,
        }
    }
}

async fn app(url: &str) -> App {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query("INSERT INTO broker_settings(singleton,adapter_url,use_ollama_api) VALUES(1,?,1)")
        .bind(url)
        .execute(&db)
        .await
        .unwrap();
    for (id, role) in [("admin", "admin"), ("user", "user")] {
        sqlx::query("INSERT INTO users(id,username,password_hash,role) VALUES(?,?,'unused',?)")
            .bind(id)
            .bind(id)
            .bind(role)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sessions(token,user_id) VALUES(?,?)")
            .bind(id)
            .bind(id)
            .execute(&db)
            .await
            .unwrap();
    }
    let (deltas, _) = broadcast::channel(32);
    App {
        db,
        adapters: test_adapter_host(),
        cancellations: Arc::new(Mutex::new(HashMap::new())),
        snapshot_gate: Arc::new(RwLock::new(())),
        memory_jobs: Arc::new(Mutex::new(HashMap::new())),
        deltas,
        recent_errors: Arc::new(Mutex::new(Vec::new())),
        log_path: Arc::new(PathBuf::new()),
        argon_gate: Arc::new(Semaphore::new(ARGON2_CONCURRENCY)),
        image_key: Arc::new([7; 32]),
    }
}

async fn start(
    app: &App,
    connection: &str,
    id: u64,
    action: OllamaAction,
    token: &str,
    cancel_immediately: bool,
) -> (tokio::task::JoinHandle<Result<()>>, mpsc::Receiver<Out>) {
    let request = Request::AdminOllamaAction {
        session_token: token.into(),
        action,
    };
    let control =
        dispatch_control(app, connection, id, &request, Arc::new(AtomicU64::new(0))).await;
    if cancel_immediately {
        let _ = app
            .cancellations
            .lock()
            .await
            .get(&(connection.into(), id))
            .unwrap()
            .send(true);
    }
    let app = app.clone();
    let connection = connection.to_owned();
    let (tx, rx) = mpsc::channel(32);
    let task = tokio::spawn(async move {
        let result = dispatch(
            app.clone(),
            tx,
            Arc::new(RwLock::new(None)),
            connection.clone(),
            id,
            request,
            control,
        )
        .await;
        app.cancellations.lock().await.remove(&(connection, id));
        result
    });
    (task, rx)
}

async fn response(rx: &mut mpsc::Receiver<Out>, id: u64) -> Response {
    let (kind, actual_id, bytes) = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kind, MessageType::Response);
    assert_eq!(actual_id, id);
    decode(&bytes).unwrap()
}

#[tokio::test]
async fn parallel_downloads_report_progress_allow_deletion_and_cancel_independently() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut inference = Inference::start().await;
        let app = app(&inference.address).await;
        let (a, mut a_events) = start(&app, "owner", 1, OllamaAction::Pull { model: "a".into() }, "admin", false).await;
        let (b, mut b_events) = start(&app, "owner", 2, OllamaAction::Pull { model: "b".into() }, "admin", false).await;
        assert!(matches!(response(&mut a_events, 1).await, Response::ModelDownloadProgress(progress) if progress.total == Some(100) && progress.completed == Some(25)));
        assert!(matches!(response(&mut b_events, 2).await, Response::ModelDownloadProgress(progress) if progress.completed == Some(25)));
        let first = inference.pulls.recv().await.unwrap();
        let second = inference.pulls.recv().await.unwrap();
        let (a_pull, b_pull) = if first.model == "a" { (first, second) } else { (second, first) };
        // Downloads must not hold the snapshot write gate for their lifetime.
        drop(app.snapshot_gate.try_write().expect("download blocked snapshots"));
        let (delete, mut deleted) = start(&app, "owner", 3, OllamaAction::Delete { model: "unrelated".into() }, "admin", false).await;
        assert!(matches!(response(&mut deleted, 3).await, Response::OllamaState(_)));
        delete.await.unwrap().unwrap();
        assert_eq!(inference.deleted.recv().await.unwrap(), "unrelated");
        assert!(!a.is_finished() && !b.is_finished());
        cancel_connection(&app.cancellations, "another-connection".into()).await;
        assert_eq!(app.cancellations.lock().await.len(), 2);
        let _ = app.cancellations.lock().await.remove(&("owner".into(), 1)).unwrap().send(true);
        assert!(matches!(response(&mut a_events, 1).await, Response::ModelDownloadFinished { cancelled: true }));
        a.await.unwrap().unwrap();
        a_pull.closed.await.unwrap();
        assert!(!b.is_finished(), "cancelling a cancelled b");
        b_pull.finish.send(()).unwrap();
        assert!(matches!(response(&mut b_events, 2).await, Response::ModelDownloadProgress(progress) if progress.status == "success"));
        assert!(matches!(response(&mut b_events, 2).await, Response::ModelDownloadFinished { cancelled: false }));
        b.await.unwrap().unwrap();
        assert!(app.cancellations.lock().await.is_empty());
    }).await.unwrap();
}

#[tokio::test]
async fn downloads_handle_early_cancel_disconnect_permissions_and_truncated_streams() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut inference = Inference::start().await;
        let app = app(&inference.address).await;
        let (early, mut events) = start(
            &app,
            "early",
            1,
            OllamaAction::Pull {
                model: "early".into(),
            },
            "admin",
            true,
        )
        .await;
        assert!(matches!(
            response(&mut events, 1).await,
            Response::ModelDownloadFinished { cancelled: true }
        ));
        early.await.unwrap().unwrap();
        assert!(
            inference.pulls.try_recv().is_err(),
            "early cancellation contacted upstream"
        );
        let (denied, _) = start(
            &app,
            "user",
            2,
            OllamaAction::Pull {
                model: "denied".into(),
            },
            "user",
            false,
        )
        .await;
        assert!(denied.await.unwrap().is_err());
        let (broken, mut events) = start(
            &app,
            "broken",
            3,
            OllamaAction::Pull {
                model: "broken".into(),
            },
            "admin",
            false,
        )
        .await;
        assert!(matches!(
            response(&mut events, 3).await,
            Response::ModelDownloadProgress(_)
        ));
        assert!(
            broken
                .await
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("before reporting success")
        );
        let (active, mut events) = start(
            &app,
            "disconnect",
            4,
            OllamaAction::Pull {
                model: "active".into(),
            },
            "admin",
            false,
        )
        .await;
        assert!(matches!(
            response(&mut events, 4).await,
            Response::ModelDownloadProgress(_)
        ));
        let pull = inference.pulls.recv().await.unwrap();
        cancel_connection(&app.cancellations, "disconnect".into()).await;
        assert!(matches!(
            response(&mut events, 4).await,
            Response::ModelDownloadFinished { cancelled: true }
        ));
        active.await.unwrap().unwrap();
        pull.closed.await.unwrap();
        // Both success and error paths must return their slots.
        let config = load_broker_config(&app.db).await.unwrap();
        let guards = (0..4)
            .map(|index| {
                app.adapters
                    .reserve_model_operation(&config, &format!("slot-{index}"), true)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            app.adapters
                .reserve_model_operation(&config, "overflow", true)
                .is_err()
        );
        assert!(
            app.adapters
                .reserve_model_operation(&config, "slot-0", false)
                .is_err()
        );
        assert!(
            app.adapters
                .reserve_model_operation(&config, "library/slot-0:latest", false)
                .is_err()
        );
        assert!(
            app.adapters
                .reserve_model_operation(&config, "unrelated", false)
                .is_ok()
        );
        drop(guards);
        assert!(
            app.adapters
                .reserve_model_operation(&config, "overflow", true)
                .is_ok()
        );
        assert!(app.cancellations.lock().await.is_empty());
    })
    .await
    .unwrap();
}
