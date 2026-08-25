//! Throwaway end-to-end smoke client: drives the real broker over TLS.
//! Not committed; lives only for this manual verification run.

use chatty_protocol::util::{bail, format_err, Context, Result};
use chatty_protocol::*;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::TlsConnector;

fn t() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        % 1000.0
}

macro_rules! ok {
    ($($a:tt)*) => { println!("[{:08.3}] [ok] {}", t(), format_args!($($a)*)) };
}

struct Link {
    stream: tokio_rustls::client::TlsStream<tokio::net::TcpStream>,
    codec: ProtocolCodec,
    next_id: u64,
}

impl Link {
    async fn connect() -> Result<Self> {
        let ca = chatty_protocol::util::pemfile::certs(&mut std::io::BufReader::new(
            std::fs::File::open("certs/ca.pem")?,
        ))
        .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in ca {
            roots.add(cert)?;
        }
        let config = Arc::new(
            rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_root_certificates(roots)
                .with_no_client_auth(),
        );
        let tcp = tokio::net::TcpStream::connect(std::env::var("CHATTY_E2E_ADDR").unwrap_or_else(|_| "127.0.0.1:19443".into())).await?;
        tcp.set_nodelay(true)?;
        let name = ServerName::try_from("127.0.0.1".to_string())?;
        let stream = TlsConnector::from(config).connect(name, tcp).await?;
        let mut link = Self {
            stream,
            codec: ProtocolCodec::new()?,
            next_id: 0,
        };
        // Handshake is the one JSON frame, sent unsolicited.
        let hello = read_frame(&mut link.stream).await?;
        let value: serde_json::Value = serde_json::from_slice(&hello.payload)?;
        if hello.message_type != MessageType::Handshake || value["protocol"] != 9 {
            bail!("bad handshake");
        }
        Ok(link)
    }

    async fn rpc(&mut self, request: Request) -> Result<Response> {
        self.next_id += 1;
        let id = self.next_id;
        self.codec
            .write_message(&mut self.stream, MessageType::Request, id, &request)
            .await?;
        loop {
            let frame = self.codec.read_frame(&mut self.stream).await?;
            match frame.message_type {
                MessageType::Response if frame.request_id == id => {
                    return decode::<Response>(&frame.payload)
                        .context("decode response");
                }
                MessageType::Error => {
                    let error: WireError = decode(&frame.payload)?;
                    bail!("wire error {:?}: {}", error.code, error.message);
                }
                _ => continue,
            }
        }
    }
}

fn expect_accepted(response: Response) -> Result<(Option<String>, i64)> {
    match response {
        Response::Accepted {
            entity_id,
            revision,
        } => Ok((entity_id, revision)),
        other => bail!("expected Accepted, got {other:?}"),
    }
}

#[tokio::main(flavor = "current_thread")]

async fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| format_err!("tls provider"))?;

    // ---- main flow on connection #1 -------------------------------------
    let mut link = Link::connect().await.context("connect")?;

    let token = match link
        .rpc(Request::Register {
            username: "e2e-user".into(),
            password: "long-e2e-password-123".into(),
        })
        .await
    {
        Ok(Response::Authenticated { session_token, .. }) => session_token,
        Ok(other) => bail!("register -> {other:?}"),
        Err(_) => {
            // rerun against a warm db: fall back to login
            match link
                .rpc(Request::Login {
                    username: "e2e-user".into(),
                    password: "long-e2e-password-123".into(),
                })
                .await?
            {
                Response::Authenticated { session_token, .. } => session_token,
                other => bail!("login -> {other:?}"),
            }
        }
    };
    ok!("register/login, TOKEN={token}");

    let (character_id, _) = expect_accepted(
        link.rpc(Request::UpsertCharacter {
            session_token: token.clone(),
            character: CharacterInput {
                id: None,
                name: "E2E Bard".into(),
                description: String::new(),
                personality: "cheerful".into(),
                scenario: "a test stage".into(),
                system_prompt: "You are testing Chatty.".into(),
                example_dialogue: String::new(),
                appearance: String::new(),
                age: String::new(),
                gender: String::new(),
                race: String::new(),
                misc: String::new(),
                tags: vec!["e2e".into()],
                avatar: None,
                is_public: false,
                owned_by_user: false,
            },
        })
        .await?,
    )?;
    let character_id = character_id.context("character id")?;
    ok!("upsert character {character_id}");

    let (conversation_id, _) = expect_accepted(
        link.rpc(Request::CreateConversation {
            session_token: token.clone(),
            title: "e2e conversation".into(),
            kind: ConversationKind::Direct,
            participant_ids: vec![character_id.clone()],
        })
        .await?,
    )?;
    let conversation_id = conversation_id.context("conversation id")?;
    ok!("create conversation {conversation_id}");

    let (_, revision) = expect_accepted(
        link.rpc(Request::SendMessage {
            session_token: token.clone(),
            conversation_id: conversation_id.clone(),
            content: "Say hello to the smoke test.".into(),
            speaker_id: None,
        })
        .await?,
    )?;
    ok!("send message at revision {revision}");

    // Generate: streamed through mock-llama via the persistent codec path.
    link.next_id += 1;
    let generate_id = link.next_id;
    link.codec
        .write_message(
            &mut link.stream,
            MessageType::Request,
            generate_id,
            &Request::Generate {
                session_token: token.clone(),
                conversation_id: conversation_id.clone(),
                speaker_id: Some(character_id.clone()),
                parent_id: None,
            },
        )
        .await?;
    let mut chunks = 0u32;
    let mut finished = false;
    let mut last_revision = revision;
    loop {
        let frame = link.codec.read_frame(&mut link.stream).await?;
        match frame.message_type {
            MessageType::StreamChunk => chunks += 1,
            MessageType::StreamEnd => {
                let finished_response: Response = decode(&frame.payload)?;
                match finished_response {
                    Response::GenerationFinished { revision, .. } => {
                        ok!("generation finished at r{revision} after {chunks} chunks");
                        last_revision = last_revision.max(revision);
                        finished = true;
                        break;
                    }
                    other => bail!("stream end -> {other:?}"),
                }
            }
            MessageType::Response if frame.request_id == generate_id => {
                match decode::<Response>(&frame.payload)? {
                    Response::GenerationStarted { .. } => ok!("generation started"),
                    Response::GenerationFinished { revision, .. } => {
                        ok!("generation finished at r{revision} after {chunks} chunks");
                        last_revision = last_revision.max(revision);
                        finished = true;
                        break;
                    }
                    other => bail!("generate -> {other:?}"),
                }
            }
            MessageType::Error => {
                let error: WireError = decode(&frame.payload)?;
                bail!("generate wire error {:?}: {}", error.code, error.message);
            }
            _ => {}
        }
    }
    assert!(finished && chunks > 0);

    let before_sync = std::time::Instant::now();
    match link
        .rpc(Request::Sync {
            session_token: token.clone(),
            since_revision: 0,
        })
        .await?
    {
        Response::SyncComplete { revision } => {
            ok!("sync to r{revision} in {:?}", before_sync.elapsed());
            last_revision = last_revision.max(revision);
        }
        other => bail!("sync -> {other:?}"),
    }

    // ---- reconnect + Resume on connection #2 ----------------------------
    drop(link);
    let mut link = Link::connect().await?;
    link.next_id += 1;
    link.codec
        .write_message(
            &mut link.stream,
            MessageType::Request,
            link.next_id,
            &Request::Resume {
                session_token: token.clone(),
                since_revision: last_revision,
            },
        )
        .await?;
    let frame = link.codec.read_frame(&mut link.stream).await?;
    let response: Response = decode(&frame.payload)?;
    match response {
        Response::Authenticated { role, .. } => {
            ok!("resumed as {role:?}");
        }
        other => bail!("resume -> {other:?}"),
    }
    // No deltas should be pending beyond our revision.
    let frame = link.codec.read_frame(&mut link.stream).await?;
    let response: Response = decode(&frame.payload)?;
    match response {
        Response::SyncComplete { revision } => {
            ok!("resume clean at r{revision}");
        }
        other => bail!("resume sync -> {other:?}"),
    }
    drop(link);
    println!("TOKEN_EXPORT={token}");

    // ---- idle-close probe on connection #3 ------------------------------
    // Login then go silent; broker must close us around IDLE_CLOSE (120s).
    let mut quiet = Link::connect().await?;
    match quiet
        .rpc(Request::Login {
            username: "e2e-user".into(),
            password: "long-e2e-password-123".into(),
        })
        .await?
    {
        Response::Authenticated { .. } => ok!("idle probe logged in; going silent"),
        other => bail!("probe login -> {other:?}"),
    }
    let start = std::time::Instant::now();
    loop {
        match read_frame(&mut quiet.stream).await {
            Err(ProtocolError::Io(io))
                if io.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                println!(
                    "[ok] idle-closed by broker after {:.1}s",
                    start.elapsed().as_secs_f64()
                );
                break;
            }
            Err(e) => bail!("probe read: {e}"),
            Ok(_) => continue,
        }
    }

    println!("E2E PASS");
    Ok(())
}

