//! Broker connection task: TLS, session persistence, and request routing.

mod address;
mod session;
#[cfg(test)]
mod tests;
mod tls;

pub(crate) use address::{ConnectionTarget, connection_target};
pub(crate) use session::{
    SpeechPreferences, last_server_path, load_last_server, load_session, load_speech_preferences,
    preferences_path, save_preferences, session_path,
};
pub(crate) use tls::PendingCertificate;

use crate::Args;
use chatty_protocol::util::current_utc_timestamp;
use chatty_protocol::{
    ErrorCode, Frame, MessageType, ProtocolCodec, Request, Response, WireError, decode,
};
use session::{SavedSession, save_last_server, save_session};
use std::collections::{HashMap, VecDeque};
use std::fs::{self};
use std::time::Duration;
use tls::{ConnectAttempt, connect, save_trusted_certificate};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub(crate) enum Command {
    Connect(ConnectionTarget),
    TrustCertificate(PendingCertificate),
    RejectCertificate,
    Disconnect,
    Request(Box<Request>),
    SendThenGenerate {
        message: Box<Request>,
        generate: Box<Request>,
    },
    Cancel(u64),
    ClearSession,
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    ModelRequestStarted(u64),
    EmotionInstallStarted(u64),
    DownloadStarted {
        request_id: u64,
        model: String,
    },
    /// A cancellable image job was sent. `scene` distinguishes chat images from portraits.
    ImageRequestStarted {
        request_id: u64,
        scene: bool,
    },
    Status(String),
    Connected {
        resuming_session: bool,
    },
    ConnectionFailed(String),
    CertificateTrustRequired(PendingCertificate),
    Disconnected,
    Frame(Frame),
    SessionExpired,
}

pub(crate) type EventSender = mpsc::UnboundedSender<Event>;

/// Favors queued commands over the live channel so nothing typed while the
/// link is down is lost.
async fn next_command(
    pending: &mut VecDeque<Command>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> Option<Command> {
    if let Some(command) = pending.pop_front() {
        return Some(command);
    }
    commands.recv().await
}

/// Waits for user action instead of holding an idle wire. Returns `None`
/// only when the app dropped its command sender.
async fn park_for_command(
    pending: &mut VecDeque<Command>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
) -> Option<()> {
    pending.push_back(commands.recv().await?);
    Some(())
}

pub(crate) async fn run(
    args: Args,
    mut remembered: Option<SavedSession>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: EventSender,
) {
    let path = match session_path(&args) {
        Ok(path) => path,
        Err(error) => {
            let _ = events.send(Event::Status(format!(
                "{} · Startup failed: {error:#}",
                current_utc_timestamp()
            )));
            return;
        }
    };
    let mut next_id = 1u64;
    let mut signed_out_through_request = None;
    let mut target: Option<ConnectionTarget> = None;
    let mut established_for_target = false;
    let mut pending = VecDeque::<Command>::new();
    let last_server = last_server_path(&path);
    loop {
        if target.is_none() {
            match commands.recv().await {
                Some(Command::Connect(requested)) => {
                    target = Some(requested);
                    established_for_target = false;
                }
                Some(Command::ClearSession) => {
                    remembered = None;
                    let _ = fs::remove_file(&path);
                    continue;
                }
                None => return,
                _ => continue,
            }
        }

        let active_target = target.clone().expect("connection target is set");
        let _ = events.send(Event::Status("Connecting…".into()));
        let mut stream = match tokio::time::timeout(
            Duration::from_secs(8),
            connect(&args, &active_target),
        )
        .await
        {
            Ok(Ok(ConnectAttempt::Connected(stream))) => *stream,
            Ok(Ok(ConnectAttempt::TrustRequired(pending_certificate))) => {
                let _ = events.send(Event::CertificateTrustRequired(pending_certificate.clone()));
                match commands.recv().await {
                    Some(Command::TrustCertificate(accepted))
                        if accepted == pending_certificate =>
                    {
                        if let Err(error) = save_trusted_certificate(&accepted) {
                            target = None;
                            let _ = events.send(Event::ConnectionFailed(format!(
                                "Could not save trusted certificate: {error:#}"
                            )));
                        }
                    }
                    Some(Command::Connect(requested)) => {
                        target = Some(requested);
                    }
                    None => return,
                    Some(Command::RejectCertificate) | Some(Command::Disconnect) => {
                        target = None;
                        let _ = events.send(Event::Disconnected);
                    }
                    _ => {
                        target = None;
                        let _ = events.send(Event::ConnectionFailed(
                            "The broker certificate was not trusted.".into(),
                        ));
                    }
                }
                continue;
            }
            Ok(Err(error)) => {
                if established_for_target {
                    let _ = events.send(Event::Status(format!(
                        "{} · Offline: {error:#}",
                        current_utc_timestamp()
                    )));
                    if park_for_command(&mut pending, &mut commands)
                        .await
                        .is_none()
                    {
                        return;
                    }
                } else {
                    target = None;
                    let _ = events.send(Event::ConnectionFailed(format!(
                        "Could not connect: {error:#}"
                    )));
                }
                continue;
            }
            Err(_) => {
                if established_for_target {
                    let _ = events.send(Event::Status(format!(
                        "{} · Offline: connection timed out",
                        current_utc_timestamp()
                    )));
                    if park_for_command(&mut pending, &mut commands)
                        .await
                        .is_none()
                    {
                        return;
                    }
                } else {
                    target = None;
                    let _ = events.send(Event::ConnectionFailed(
                        "Could not connect: the server did not respond within 8 seconds".into(),
                    ));
                }
                continue;
            }
        };

        established_for_target = true;
        save_last_server(&last_server, &active_target.server_name);
        // Persistent zstd contexts + scratch buffers for this connection.
        let mut codec = match ProtocolCodec::new() {
            Ok(codec) => codec,
            Err(error) => {
                let _ = events.send(Event::ConnectionFailed(format!(
                    "Could not connect: {error}"
                )));
                target = None;
                continue;
            }
        };
        let resume_token = remembered
            .as_ref()
            .filter(|session| session.broker == active_target.broker)
            .map(|session| session.token.clone());
        let _ = events.send(Event::Connected {
            resuming_session: resume_token.is_some(),
        });
        let _ = events.send(Event::Status("Online · TLS 1.3".into()));
        next_id += 1;
        let _ = codec
            .write_message(
                &mut stream,
                MessageType::Request,
                next_id,
                &Request::GetServerCapabilities,
            )
            .await;
        if let Some(token) = resume_token {
            next_id += 1;
            let _ = codec
                .write_message(
                    &mut stream,
                    MessageType::Request,
                    next_id,
                    &Request::Resume {
                        session_token: token,
                        since_revision: 0,
                    },
                )
                .await;
        }
        let mut reconnect = false;
        let mut pending_generations = HashMap::<u64, Box<Request>>::new();
        loop {
            tokio::select! {
                command = next_command(&mut pending, &mut commands) => match command {
                    Some(Command::Connect(_)) => {}
                    Some(Command::TrustCertificate(_)) | Some(Command::RejectCertificate) => {}
                    Some(Command::Disconnect) => {
                        target = None;
                        established_for_target = false;
                        let _ = events.send(Event::Disconnected);
                        break;
                    }
                    Some(Command::Request(request)) => {
                        next_id += 1;
                        if let Request::AdminOllamaAction { action: chatty_protocol::OllamaAction::Pull { model }, .. } = &*request {
                            let _ = events.send(Event::DownloadStarted { request_id: next_id, model: model.clone() });
                        } else if matches!(&*request, Request::AdminOllamaAction { .. } | Request::AdminSearchModels { .. } | Request::AdminGetOllamaState { .. }) {
                            let _ = events.send(Event::ModelRequestStarted(next_id));
                        } else if matches!(&*request, Request::AdminInstallEmotionModel { .. }) {
                            let _ = events.send(Event::EmotionInstallStarted(next_id));
                        } else if let Request::GenerateImage { .. } | Request::GenerateSceneImage { .. } = &*request {
                            let scene = matches!(&*request, Request::GenerateSceneImage { .. });
                            let _ = events.send(Event::ImageRequestStarted { request_id: next_id, scene });
                        }
                        if matches!(&*request, Request::Logout { .. }) {
                            remembered = None;
                            signed_out_through_request = Some(next_id);
                            let _ = fs::remove_file(&path);
                        }
                        if codec.write_message(&mut stream, MessageType::Request, next_id, &*request).await.is_err() { reconnect = true; break; }
                    }
                    Some(Command::SendThenGenerate { message, generate }) => {
                        next_id += 1;
                        let message_request_id = next_id;
                        if codec.write_message(&mut stream, MessageType::Request, message_request_id, &*message).await.is_err() {
                            reconnect = true;
                            break;
                        }
                        pending_generations.insert(message_request_id, generate);
                    }
                    Some(Command::Cancel(id)) => if codec.write_payload(&mut stream, MessageType::Cancel, id, &[]).await.is_err() { reconnect = true; break; },
                    Some(Command::ClearSession) => {
                        remembered = None;
                        signed_out_through_request = Some(next_id);
                        let _ = fs::remove_file(&path);
                    }
                    None => return,
                },
                result = codec.read_frame(&mut stream) => match result {
                    Ok(frame) => {
                        // Decode each frame at most once in this loop.
                        let response = (frame.message_type == MessageType::Response)
                            .then(|| decode::<Response>(&frame.payload).ok())
                            .flatten();
                        let error = (frame.message_type == MessageType::Error)
                            .then(|| decode::<WireError>(&frame.payload).ok())
                            .flatten();
                        let pending_generation = take_accepted_generation(
                            response.as_ref(),
                            frame.request_id,
                            frame.message_type == MessageType::Error,
                            &mut pending_generations,
                        );
                        if let Some(Response::Authenticated { session_token, .. }) = response { let session = SavedSession { broker: active_target.broker.clone(), token: session_token }; remembered = Some(session.clone()); signed_out_through_request = None; if !args.inspect { save_session(&path, &session); } }
                        if let Some(error) = &error && matches!(error.code, ErrorCode::Unauthorized) && remembered.as_ref().is_some_and(|session| session.broker == active_target.broker) { remembered = None; let _ = fs::remove_file(&path); let _ = events.send(Event::SessionExpired); }
                        if is_expected_post_logout_unauthorized(frame.request_id, error.as_ref(), signed_out_through_request) {
                            continue;
                        }
                        if events.send(Event::Frame(frame)).is_err() { return; }
                        if let Some(generate) = pending_generation {
                            next_id += 1;
                            if codec.write_message(&mut stream, MessageType::Request, next_id, &*generate).await.is_err() {
                                reconnect = true;
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = events.send(Event::Status(connection_closed_status(&e)));
                        reconnect = true;
                        break;
                    }
                }
            }
        }
        if reconnect && target.is_some() {
            // The broker closes quiet connections; go on-demand until the
            // client acts again.
            if park_for_command(&mut pending, &mut commands)
                .await
                .is_none()
            {
                return;
            }
        }
    }
}

fn connection_closed_status(error: &chatty_protocol::ProtocolError) -> String {
    if matches!(error, chatty_protocol::ProtocolError::Io(io)
        if io.kind() == std::io::ErrorKind::UnexpectedEof)
    {
        "Idle · reconnects on demand".into()
    } else {
        format!("{} · Offline: {error}", current_utc_timestamp())
    }
}

fn take_accepted_generation(
    response: Option<&Response>,
    request_id: u64,
    is_error_frame: bool,
    pending: &mut HashMap<u64, Box<Request>>,
) -> Option<Box<Request>> {
    match response {
        Some(Response::Accepted { .. }) => pending.remove(&request_id),
        _ if is_error_frame => {
            pending.remove(&request_id);
            None
        }
        _ => None,
    }
}

fn is_expected_post_logout_unauthorized(
    request_id: u64,
    error: Option<&WireError>,
    cutoff: Option<u64>,
) -> bool {
    cutoff.is_some_and(|cutoff| request_id <= cutoff)
        && error.is_some_and(|error| matches!(error.code, ErrorCode::Unauthorized))
}
