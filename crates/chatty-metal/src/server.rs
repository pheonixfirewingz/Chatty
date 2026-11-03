//! The broker-facing server. One TCP port speaks the private Chatty appliance
//! protocol; a debug image also answers its HTTP status and agent paths there.
//!
//! The first byte separates the two: a protocol frame starts with the high byte
//! of a payload length below 16 MiB, which is zero; an HTTP method starts with an
//! ASCII letter. Production images never enable the HTTP path.

use crate::{
    Result,
    catalog::Catalog,
    gpu_model::{self, GpuModel, Missing, Policy},
    tokenizer::Tokenizer,
};
use chatty_metal_protocol as wire;
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};

pub const LISTEN: &str = "0.0.0.0:11434";
const MAX_REQUEST_BYTES: usize = 4096;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 64;
const JOB_QUEUE: usize = 16;
const REPLY_QUEUE: usize = 8;

const BOS: &str = "<\u{ff5c}begin\u{2581}of\u{2581}sentence\u{ff5c}>";
const USER: &str = "<\u{ff5c}User\u{ff5c}>";
const ASSISTANT: &str = "<\u{ff5c}Assistant\u{ff5c}>";
const EOS: &str = "<\u{ff5c}end\u{2581}of\u{2581}sentence\u{ff5c}>";

/// A single-model worker. One generation runs at a time; connections queue.
struct StatusBoard {
    started: Instant,
    state: RwLock<wire::ServerState>,
    detail: RwLock<String>,
    model: RwLock<String>,
    context_tokens: u32,
    tokens_generated: AtomicU64,
    requests_served: AtomicU64,
    last_decode_tps: AtomicU64,
    queue_depth: AtomicUsize,
}

impl StatusBoard {
    fn new(model: String) -> Self {
        Self {
            started: Instant::now(),
            state: RwLock::new(wire::ServerState::Loading),
            detail: RwLock::new("loading the model".into()),
            model: RwLock::new(model),
            context_tokens: gpu_model::CONTEXT as u32,
            tokens_generated: AtomicU64::new(0),
            requests_served: AtomicU64::new(0),
            last_decode_tps: AtomicU64::new(0),
            queue_depth: AtomicUsize::new(0),
        }
    }

    fn set(&self, state: wire::ServerState, detail: impl Into<String>) {
        if let Ok(mut slot) = self.state.write() {
            *slot = state;
        }
        if let Ok(mut slot) = self.detail.write() {
            *slot = detail.into();
        }
    }

    fn snapshot(&self) -> wire::Status {
        wire::Status {
            state: self
                .state
                .read()
                .map(|s| *s)
                .unwrap_or(wire::ServerState::Failed),
            detail: self.detail.read().map(|d| d.clone()).unwrap_or_default(),
            model: self.model.read().map(|m| m.clone()).unwrap_or_default(),
            context_tokens: self.context_tokens,
            uptime_ms: self.started.elapsed().as_millis() as u64,
            tokens_generated: self.tokens_generated.load(Ordering::Relaxed),
            requests_served: self.requests_served.load(Ordering::Relaxed),
            last_decode_tokens_per_second: f32::from_bits(
                self.last_decode_tps.load(Ordering::Relaxed) as u32,
            ),
            queue_depth: self.queue_depth.load(Ordering::Relaxed) as u32,
        }
    }

    fn hello_ack(&self) -> wire::HelloAck {
        let status = self.snapshot();
        wire::HelloAck {
            version: wire::PROTOCOL_VERSION,
            model: status.model,
            context_tokens: status.context_tokens,
            max_output_tokens: wire::MAX_OUTPUT_TOKENS,
            state: status.state,
            detail: status.detail,
        }
    }

    fn status_text(&self) -> String {
        let status = self.snapshot();
        format!(
            "chatty-metal protocol v{}\nstate: {:?}\ndetail: {}\nmodel: {}\ncontext: {} tokens\nuptime: {} ms\ntokens: {}\nrequests: {}\nlast decode: {:.2} tokens/s\n",
            wire::PROTOCOL_VERSION,
            status.state,
            status.detail,
            status.model,
            status.context_tokens,
            status.uptime_ms,
            status.tokens_generated,
            status.requests_served,
            status.last_decode_tokens_per_second
        )
    }
}

struct Job {
    request: wire::Generate,
    cancel: Arc<AtomicBool>,
    replies: SyncSender<Event>,
}

enum Event {
    Chunk(wire::Chunk),
    Done(wire::Done),
    Error(wire::WireError),
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Start the model worker and accept connections forever.
pub fn serve(model_dir: Option<PathBuf>, boot_report: &str) -> ! {
    serve_with(LISTEN, boot_report, model_dir)
}

pub fn serve_on(address: &str, boot_report: &str) -> ! {
    serve_with(address, boot_report, None)
}

fn serve_with(address: &str, boot_report: &str, model_dir: Option<PathBuf>) -> ! {
    let model = model_dir
        .as_deref()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no model".into());
    let board = Arc::new(StatusBoard::new(model));
    let (jobs, queue) = sync_channel(JOB_QUEUE);
    if let Some(dir) = model_dir {
        let worker_board = Arc::clone(&board);
        if let Err(error) = thread::Builder::new()
            .name("model".into())
            .spawn(move || run_engine(dir, worker_board, queue))
        {
            board.set(
                wire::ServerState::Failed,
                format!("model worker did not start: {error}"),
            );
        }
    } else {
        board.set(wire::ServerState::Failed, "no model directory");
    }

    let connections = Arc::new(AtomicUsize::new(0));
    loop {
        let listener = match TcpListener::bind(address) {
            Ok(listener) => listener,
            Err(error) => {
                crate::boot::log_line(&format!("listen failed: {error}"));
                thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        for stream in listener.incoming().flatten() {
            if connections.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
                connections.fetch_sub(1, Ordering::Relaxed);
                continue;
            }
            let board = Arc::clone(&board);
            let jobs = jobs.clone();
            let connections = Arc::clone(&connections);
            let report = boot_report.to_owned();
            let spawned = thread::Builder::new()
                .name("connection".into())
                .spawn(move || {
                    let _count = ConnectionCount(connections);
                    let _ = dispatch(stream, &board, &jobs, &report);
                });
            if spawned.is_err() {
                // A failed spawn drops the connection and its count guard.
            }
        }
    }
}

struct ConnectionCount(Arc<AtomicUsize>);
impl Drop for ConnectionCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn dispatch(
    stream: TcpStream,
    board: &Arc<StatusBoard>,
    jobs: &SyncSender<Job>,
    boot_report: &str,
) -> io::Result<()> {
    let _ = stream.set_read_timeout(Some(CLIENT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_TIMEOUT));
    let mut first = [0u8; 1];
    if stream.peek(&mut first)? == 0 {
        return Ok(());
    }
    if first[0] == 0 {
        handle_protocol(stream, board, jobs).map_err(io::Error::other)
    } else {
        handle_http(stream, board, boot_report)
    }
}

fn handle_protocol(
    stream: TcpStream,
    board: &Arc<StatusBoard>,
    jobs: &SyncSender<Job>,
) -> wire::Result<()> {
    let mut reader = stream.try_clone()?;
    let mut writer = stream;
    let frame = match wire::read_frame(&mut reader) {
        Ok(frame) => frame,
        Err(_) => return Ok(()),
    };
    let hello: wire::Hello = match wire::decode(&frame.payload) {
        Ok(hello) => hello,
        Err(_) => {
            return send_error(
                &mut writer,
                frame.request_id,
                wire::ErrorCode::Protocol,
                "invalid hello",
            );
        }
    };
    if hello.validate().is_err() {
        return send_error(
            &mut writer,
            frame.request_id,
            wire::ErrorCode::Protocol,
            "unsupported protocol version",
        );
    }
    wire::write_message(
        &mut writer,
        wire::MessageType::HelloAck,
        frame.request_id,
        &board.hello_ack(),
    )?;

    loop {
        let frame = match wire::read_frame(&mut reader) {
            Ok(frame) => frame,
            Err(_) => return Ok(()),
        };
        match frame.message_type {
            wire::MessageType::StatusRequest => {
                wire::write_message(
                    &mut writer,
                    wire::MessageType::Status,
                    frame.request_id,
                    &board.snapshot(),
                )?;
            }
            wire::MessageType::Generate => {
                return handle_generate(&mut writer, board, jobs, frame);
            }
            wire::MessageType::Cancel => return Ok(()),
            _ => {
                return send_error(
                    &mut writer,
                    frame.request_id,
                    wire::ErrorCode::Protocol,
                    "unexpected message",
                );
            }
        }
    }
}

fn handle_generate(
    writer: &mut TcpStream,
    board: &Arc<StatusBoard>,
    jobs: &SyncSender<Job>,
    frame: wire::Frame,
) -> wire::Result<()> {
    let request: wire::Generate = match wire::decode(&frame.payload) {
        Ok(request) => request,
        Err(_) => {
            return send_error(
                writer,
                frame.request_id,
                wire::ErrorCode::InvalidRequest,
                "invalid generate request",
            );
        }
    };
    if let Err(error) = request.validate() {
        return send_error(
            writer,
            frame.request_id,
            wire::ErrorCode::InvalidRequest,
            &error.to_string(),
        );
    }
    if board.snapshot().state != wire::ServerState::Ready {
        return send_error(
            writer,
            frame.request_id,
            wire::ErrorCode::NotReady,
            &board.snapshot().detail,
        );
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let guard = CancelOnDrop(Arc::clone(&cancel));
    let (replies, reply) = sync_channel(REPLY_QUEUE);
    board.queue_depth.fetch_add(1, Ordering::Relaxed);
    if jobs
        .send(Job {
            request,
            cancel,
            replies,
        })
        .is_err()
    {
        board.queue_depth.fetch_sub(1, Ordering::Relaxed);
        return send_error(
            writer,
            frame.request_id,
            wire::ErrorCode::Internal,
            "model worker is not running",
        );
    }
    board.queue_depth.fetch_sub(1, Ordering::Relaxed);

    // A client that sends Cancel after Generate is not read here; dropping the
    // connection stops the worker through the failed reply send.
    let _guard = guard;
    while let Ok(event) = reply.recv() {
        match event {
            Event::Chunk(chunk) => {
                wire::write_message(writer, wire::MessageType::Chunk, frame.request_id, &chunk)?;
            }
            Event::Done(done) => {
                wire::write_message(writer, wire::MessageType::Done, frame.request_id, &done)?;
                return Ok(());
            }
            Event::Error(error) => {
                wire::write_message(writer, wire::MessageType::Error, frame.request_id, &error)?;
                return Ok(());
            }
        }
    }
    Ok(())
}

fn send_error(
    writer: &mut TcpStream,
    request_id: u64,
    code: wire::ErrorCode,
    message: &str,
) -> wire::Result<()> {
    wire::write_message(
        writer,
        wire::MessageType::Error,
        request_id,
        &wire::WireError {
            code,
            message: message.to_owned(),
        },
    )
}

fn run_engine(model_dir: PathBuf, board: Arc<StatusBoard>, jobs: Receiver<Job>) {
    let tokenizer = match Tokenizer::open(&model_dir.join("tokenizer.json")) {
        Ok(tokenizer) => tokenizer,
        Err(error) => return fail_worker(&board, jobs, error.to_string()),
    };
    let catalog = match Catalog::open(&model_dir) {
        Ok(catalog) => Arc::new(catalog),
        Err(error) => return fail_worker(&board, jobs, error.to_string()),
    };
    let mut model = match GpuModel::load(catalog, &tokenizer) {
        Ok(model) => model,
        Err(error) => return fail_worker(&board, jobs, error.to_string()),
    };
    model.set_policy(Policy {
        missing: Missing::DropRenormalized,
        host_budget: 4,
        must_run: 2,
        ram_ranks: 4,
    });
    let _ = model.preload_hot();
    board.set(wire::ServerState::Ready, "ready");
    crate::boot::log_line("model ready");

    while let Ok(job) = jobs.recv() {
        generate(&mut model, &tokenizer, &board, job);
    }
}

fn fail_worker(board: &StatusBoard, jobs: Receiver<Job>, detail: String) {
    board.set(wire::ServerState::Failed, detail.clone());
    crate::boot::log_line(&format!("model failed: {detail}"));
    while let Ok(job) = jobs.recv() {
        let _ = job.replies.send(Event::Error(wire::WireError {
            code: wire::ErrorCode::NotReady,
            message: detail.clone(),
        }));
    }
}

fn generate(model: &mut GpuModel, tokenizer: &Tokenizer, board: &StatusBoard, job: Job) {
    let request = job.request;
    if let Err(error) = request.validate() {
        let _ = job.replies.send(Event::Error(wire::WireError {
            code: wire::ErrorCode::InvalidRequest,
            message: error.to_string(),
        }));
        return;
    }
    let ids = match prompt_ids(tokenizer, &request.prompt) {
        Ok(ids) => ids,
        Err(error) => {
            let _ = job.replies.send(Event::Error(wire::WireError {
                code: wire::ErrorCode::InvalidRequest,
                message: error.to_string(),
            }));
            return;
        }
    };
    if ids.len() + request.max_tokens as usize > gpu_model::CONTEXT {
        let _ = job.replies.send(Event::Error(wire::WireError {
            code: wire::ErrorCode::ContextExceeded,
            message: format!(
                "prompt of {} tokens plus {} output tokens exceeds the {} token context",
                ids.len(),
                request.max_tokens,
                gpu_model::CONTEXT
            ),
        }));
        return;
    }

    model.reset();
    let mut logits = Vec::new();
    for chunk in ids.chunks(gpu_model::MAX_CHUNK) {
        match model.forward(chunk) {
            Ok(next) => logits = next,
            Err(error) => {
                let _ = job.replies.send(Event::Error(wire::WireError {
                    code: wire::ErrorCode::Internal,
                    message: error.to_string(),
                }));
                return;
            }
        }
        if job.cancel.load(Ordering::Relaxed) {
            let _ = job.replies.send(Event::Done(wire::Done {
                finish: wire::FinishReason::Cancelled,
                prompt_tokens: ids.len() as u32,
                completion_tokens: 0,
                decode_ms: 0,
            }));
            return;
        }
    }

    let stop = match tokenizer.special(EOS) {
        Ok(id) => id,
        Err(error) => {
            let _ = job.replies.send(Event::Error(wire::WireError {
                code: wire::ErrorCode::Internal,
                message: error.to_string(),
            }));
            return;
        }
    };
    let started = Instant::now();
    let mut rng = request.sampling.seed | 1;
    let mut pending = Vec::new();
    let mut sequence = 0u32;
    let mut completion = 0u32;
    let finish;
    loop {
        if job.cancel.load(Ordering::Relaxed) {
            finish = wire::FinishReason::Cancelled;
            break;
        }
        let next = sample(&logits, request.sampling, &mut rng);
        completion += 1;
        if next == stop {
            finish = wire::FinishReason::EndOfSentence;
            break;
        }
        pending.extend(tokenizer.token_bytes(next));
        let valid = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) => error.valid_up_to(),
        };
        if valid > 0 {
            let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);
            if job
                .replies
                .send(Event::Chunk(wire::Chunk { sequence, text }))
                .is_err()
            {
                return;
            }
            sequence = sequence.wrapping_add(1);
        }
        if completion >= request.max_tokens {
            finish = wire::FinishReason::Length;
            break;
        }
        match model.forward(&[next]) {
            Ok(next) => logits = next,
            Err(error) => {
                let _ = job.replies.send(Event::Error(wire::WireError {
                    code: wire::ErrorCode::Internal,
                    message: error.to_string(),
                }));
                return;
            }
        }
    }
    let decode_ms = started.elapsed().as_millis() as u64;
    let tps = if decode_ms > 0 {
        f64::from(completion) * 1000.0 / decode_ms as f64
    } else {
        0.0
    };
    board
        .tokens_generated
        .fetch_add(u64::from(completion), Ordering::Relaxed);
    board.requests_served.fetch_add(1, Ordering::Relaxed);
    board
        .last_decode_tps
        .store(u64::from((tps as f32).to_bits()), Ordering::Relaxed);
    let _ = job.replies.send(Event::Done(wire::Done {
        finish,
        prompt_tokens: ids.len() as u32,
        completion_tokens: completion,
        decode_ms,
    }));
}

fn prompt_ids(tokenizer: &Tokenizer, pieces: &[wire::Piece]) -> Result<Vec<u32>> {
    let mut ids = Vec::new();
    for piece in pieces {
        match piece {
            wire::Piece::Text(text) => ids.extend(tokenizer.encode(text)),
            wire::Piece::Marker(marker) => {
                let name = match marker {
                    wire::Marker::BeginOfSentence => BOS,
                    wire::Marker::User => USER,
                    wire::Marker::Assistant => ASSISTANT,
                    wire::Marker::EndOfSentence => EOS,
                };
                ids.push(tokenizer.special(name)?);
            }
        }
    }
    Ok(ids)
}

/// Greedy at temperature zero. Otherwise sample after top-k/top-p filtering.
fn sample(logits: &[f32], sampling: wire::Sampling, rng: &mut u64) -> u32 {
    if sampling.temperature <= 0.0 {
        return logits
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(id, _)| id as u32)
            .unwrap_or(0);
    }
    let mut candidates: Vec<(f32, u32)> = logits
        .iter()
        .enumerate()
        .map(|(id, &logit)| (logit, id as u32))
        .collect();
    let limit = if sampling.top_k == 0 {
        candidates.len()
    } else {
        (sampling.top_k as usize).min(candidates.len())
    };
    if limit < candidates.len() {
        candidates.select_nth_unstable_by(limit - 1, |a, b| b.0.total_cmp(&a.0));
        candidates.truncate(limit);
    }
    candidates.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
    let max = candidates.first().map(|(logit, _)| *logit).unwrap_or(0.0);
    let mut weights: Vec<f32> = candidates
        .iter()
        .map(|(logit, _)| ((logit - max) / sampling.temperature).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return candidates.first().map(|(_, id)| *id).unwrap_or(0);
    }
    for weight in &mut weights {
        *weight /= total;
    }
    let mut cumulative = 0.0;
    let mut keep = weights.len();
    for (index, weight) in weights.iter().enumerate() {
        cumulative += weight;
        if cumulative >= sampling.top_p {
            keep = index + 1;
            break;
        }
    }
    let total: f32 = weights[..keep].iter().sum();
    let mut pick = next_uniform(rng) * f64::from(total);
    for (index, weight) in weights[..keep].iter().enumerate() {
        pick -= f64::from(*weight);
        if pick <= 0.0 {
            return candidates[index].1;
        }
    }
    candidates[keep.saturating_sub(1)].1
}

fn next_uniform(rng: &mut u64) -> f64 {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    ((*rng >> 11) as f64 + 0.5) / (1u64 << 53) as f64
}

fn handle_http(mut stream: TcpStream, board: &StatusBoard, boot_report: &str) -> io::Result<()> {
    let mut head = Vec::with_capacity(512);
    let mut buffer = [0; 512];
    let body_start = loop {
        let len = stream.read(&mut buffer)?;
        if len == 0 {
            break head.len();
        }
        head.extend_from_slice(&buffer[..len]);
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if head.len() > MAX_REQUEST_BYTES {
            break head.len();
        }
    };

    #[cfg(feature = "debug-agent")]
    {
        use crate::agent::{self, Head};
        let text = String::from_utf8_lossy(&head[..body_start]);
        if let Some(parsed) = Head::parse(&text).filter(|parsed| parsed.path.starts_with("/debug/"))
        {
            let mut reader = stream.try_clone()?;
            let body = io::Cursor::new(&head[body_start..]).chain(&mut reader);
            return agent::handle(&mut stream, body, &parsed, env!("CHATTY_METAL_DEBUG_TOKEN"));
        }
    }
    let _ = body_start;
    let mut status = board.status_text();
    if !boot_report.is_empty() {
        status.push_str("\nboot:\n");
        status.push_str(boot_report);
    }
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{status}",
        status.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_frames_are_distinct_from_http() {
        assert_eq!(wire::HEADER_LEN, 14);
        let mut frame = [0u8; wire::HEADER_LEN];
        frame[..4].copy_from_slice(&32u32.to_be_bytes());
        assert_eq!(frame[0], 0);
        assert!(b"GET / HTTP/1.1".first().is_some_and(|b| *b != 0));
    }

    #[test]
    fn sampling_is_greedy_at_zero_temperature() {
        let logits = [0.1, 4.0, 2.0];
        let mut rng = 1;
        let sampling = wire::Sampling {
            temperature: 0.0,
            ..wire::Sampling::default()
        };
        assert_eq!(sample(&logits, sampling, &mut rng), 1);
    }

    #[test]
    fn sampling_respects_top_k() {
        let logits = [0.0, 10.0, 9.0, -10.0];
        let mut rng = 7;
        let sampling = wire::Sampling {
            temperature: 1.0,
            top_k: 1,
            top_p: 1.0,
            seed: 7,
        };
        assert_eq!(sample(&logits, sampling, &mut rng), 1);
    }

    #[test]
    fn sampling_returns_a_valid_id_for_flat_logits() {
        let logits = [1.0; 32];
        let mut rng = 3;
        assert!(sample(&logits, wire::Sampling::default(), &mut rng) < 32);
    }
}
