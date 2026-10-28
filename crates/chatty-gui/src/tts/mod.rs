//! Local KittenTTS speech synthesis on a dedicated worker thread.

#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;

const MODEL: &str = "KittenML/kitten-tts-nano-0.8-int8";
const VOICES: [&str; 8] = [
    "Bella", "Jasper", "Luna", "Bruno", "Rosie", "Hugo", "Kiki", "Leo",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TtsEvent {
    Status(String),
    Speaking(bool),
    Error(String),
    Finished,
}

struct Command {
    id: u64,
    text: String,
    voice: &'static str,
}

#[derive(Default)]
struct State {
    id: u64,
    available: bool,
    busy: bool,
    status: String,
    error: Option<String>,
    sink: Option<Arc<rodio::Sink>>,
    sender: Option<tokio_mpsc::UnboundedSender<TtsEvent>>,
}

impl State {
    fn emit(&self, event: TtsEvent) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(event);
        }
    }

    fn set_busy(&mut self, busy: bool) {
        if self.busy != busy {
            self.busy = busy;
            self.emit(TtsEvent::Speaking(busy));
        }
    }

    fn set_status(&mut self, status: String) {
        if self.status != status {
            self.status = status;
            self.emit(TtsEvent::Status(self.status.clone()));
        }
    }

    fn set_error(&mut self, error: String) {
        self.status = error.clone();
        self.error = Some(error.clone());
        self.emit(TtsEvent::Status(error.clone()));
        self.emit(TtsEvent::Error(error));
    }

    fn cancel_current(&mut self) {
        self.id = self.id.wrapping_add(1);
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
    }

    fn stop(&mut self) {
        self.cancel_current();
        self.set_busy(false);
        self.set_status("Ready to read aloud.".into());
    }
}

pub(crate) struct LocalTts {
    commands: mpsc::Sender<Command>,
    state: Arc<Mutex<State>>,
}

impl LocalTts {
    pub(crate) fn new() -> Self {
        Self::with_optional_sender(None)
    }

    pub(crate) fn with_sender(sender: tokio_mpsc::UnboundedSender<TtsEvent>) -> Self {
        Self::with_optional_sender(Some(sender))
    }

    fn with_optional_sender(sender: Option<tokio_mpsc::UnboundedSender<TtsEvent>>) -> Self {
        let (commands, receiver) = mpsc::channel();
        let state = Arc::new(Mutex::new(State {
            available: true,
            status: "Ready. The voice model loads on first use.".into(),
            sender,
            ..State::default()
        }));
        let worker_state = state.clone();
        // A runtime panic must be visible instead of silently killing speech forever.
        let spawned = std::thread::Builder::new()
            .name("chatty-local-tts".into())
            .spawn(move || {
                if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(receiver, &worker_state);
                })) {
                    let detail = panic
                        .downcast_ref::<String>()
                        .map(String::as_str)
                        .or_else(|| panic.downcast_ref::<&str>().copied())
                        .unwrap_or("unknown runtime failure");
                    let mut state = worker_state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    state.stop();
                    let msg = format!("Speech worker failed: {detail}. Restart Chatty to retry.");
                    state.set_error(msg);
                }
                worker_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .available = false;
            });
        if let Err(error) = spawned {
            let mut state = state.lock().unwrap();
            let msg = format!("Cannot start local speech: {error}");
            state.set_error(msg);
            state.available = false;
        }
        Self { commands, state }
    }

    pub(crate) fn is_speaking(&self) -> bool {
        self.state.lock().unwrap().busy
    }

    pub(crate) fn status(&self) -> String {
        self.state.lock().unwrap().status.clone()
    }

    pub(crate) fn take_error(&self) -> Option<String> {
        self.state.lock().unwrap().error.take()
    }

    pub(crate) fn speak(&mut self, text: &str, character_id: Option<&str>) -> Result<(), String> {
        let text = speech_text(text);
        if text.is_empty() {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap();
        state.cancel_current();
        state.error = None;
        state.set_busy(true);
        state.set_status("Preparing local speech…".into());
        if self
            .commands
            .send(Command {
                id: state.id,
                text,
                voice: voice_for(character_id),
            })
            .is_err()
        {
            state.set_busy(false);
            let msg = "The local speech worker stopped. Restart Chatty to retry.".to_string();
            state.set_error(msg.clone());
            return Err(msg);
        }
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        self.state.lock().unwrap().stop();
    }
}

pub(crate) fn tts_channel() -> (LocalTts, tokio_mpsc::UnboundedReceiver<TtsEvent>) {
    let (sender, receiver) = tokio_mpsc::unbounded_channel();
    (LocalTts::with_sender(sender), receiver)
}

impl Drop for LocalTts {
    fn drop(&mut self) {
        self.stop();
    }
}

fn update_state(state: &Mutex<State>, id: u64, update: impl FnOnce(&mut State)) -> bool {
    let mut state = state.lock().unwrap();
    if state.id != id {
        return false;
    }
    update(&mut state);
    true
}

fn run(commands: mpsc::Receiver<Command>, state: &Mutex<State>) {
    let mut stream = None;
    let mut model = None;
    loop {
        let command = match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut state = state.lock().unwrap();
                if state.sink.as_ref().is_some_and(|sink| sink.empty()) {
                    state.sink = None;
                    let was_busy = state.busy;
                    state.set_busy(false);
                    state.set_status("Ready to read aloud.".into());
                    if was_busy {
                        state.emit(TtsEvent::Finished);
                    }
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let id = command.id;
        if !update_state(state, id, |_| {}) {
            continue;
        }
        let result = (|| -> Result<(), String> {
            if stream.is_none() {
                stream = Some(rodio::OutputStreamBuilder::open_default_stream().map_err(|error| {
                    format!("Cannot open audio output: {error}. Check your output device and volume. Flatpak requires audio permission (pulseaudio).")
                })?);
            }
            if model.is_none() {
                let loaded = load_model(|progress| {
                    use kittentts::download::LoadProgress;
                    let status = match progress {
                        LoadProgress::Fetching { step, total, file } =>
                            format!("Preparing voice model ({step}/{total}): {file}. First use needs internet."),
                        LoadProgress::Loading => "Loading local voice model…".into(),
                    };
                    update_state(state, id, |state| state.set_status(status));
                }).map_err(|error| format!("Cannot load the local voice model: {error:#}. Connect to the internet and try Test voice again."))?;
                model = Some(loaded);
            }
            if !update_state(state, id, |state| {
                state.set_status("Generating speech locally…".into());
            }) {
                return Ok(());
            }
            let sink = Arc::new(rodio::Sink::connect_new(stream.as_ref().unwrap().mixer()));
            if !update_state(state, id, |state| state.sink = Some(sink.clone())) {
                return Ok(());
            }
            // Start playing after the first short chunk; cancel between chunks.
            for chunk in speech_chunks(&command.text) {
                if !update_state(state, id, |_| {}) {
                    return Ok(());
                }
                let samples = model
                    .as_ref()
                    .unwrap()
                    .generate(&chunk, command.voice, 1.0, true)
                    .map_err(|error| {
                        format!("Cannot generate local speech: {error:#}. Try Test voice again.")
                    })?;
                if samples.is_empty() || samples.iter().any(|sample| !sample.is_finite()) {
                    return Err("The voice model returned invalid audio. Try another reply.".into());
                }
                // Check and append under the same lock so Stop cannot race playback.
                if !update_state(state, id, |state| {
                    sink.append(rodio::buffer::SamplesBuffer::new(1, 24_000, samples));
                    state.set_status("Speaking locally…".into());
                }) {
                    return Ok(());
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            update_state(state, id, |state| {
                if let Some(sink) = state.sink.take() {
                    sink.stop();
                }
                state.set_busy(false);
                state.set_error(error);
            });
        }
    }
}

fn speech_chunks(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut chunk = String::new();
    for word in text.split_whitespace() {
        if !chunk.is_empty() && chunk.len() + word.len() + 1 > 240 {
            chunks.push(std::mem::take(&mut chunk));
        }
        if !chunk.is_empty() {
            chunk.push(' ');
        }
        chunk.push_str(word);
        if word.ends_with(['.', '!', '?']) {
            chunks.push(std::mem::take(&mut chunk));
        }
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

fn load_model(
    mut progress: impl FnMut(kittentts::download::LoadProgress),
) -> Result<kittentts::model::KittenTtsOnnx, String> {
    use kittentts::download::{LoadProgress, ModelConfig};
    // KittenTTS's convenience downloader uses Api::new(), which ignores HF_HOME
    // and XDG_CACHE_HOME. In Flatpak that puts models in an ephemeral home folder.
    let mut builder = hf_hub::api::sync::ApiBuilder::from_env().with_progress(false);
    if std::env::var_os("HF_HOME").is_none()
        && let Some(cache) = std::env::var_os("XDG_CACHE_HOME").map(std::path::PathBuf::from)
        && cache.is_absolute()
    {
        builder = builder.with_cache_dir(cache.join("huggingface/hub"));
    }
    let api = builder
        .build()
        .map_err(|error| format!("Cannot initialize model download: {error}"))?;
    let repo = api.model(MODEL.into());
    let mut fetch = |file: &str, step| {
        progress(LoadProgress::Fetching {
            step,
            total: 4,
            file: file.into(),
        });
        repo.get(file)
            .map_err(|error| format!("Cannot fetch {file}: {error}"))
    };
    let config_path = fetch("config.json", 1)?;
    let config: ModelConfig = serde_json::from_slice(
        &std::fs::read(config_path)
            .map_err(|error| format!("Cannot read model configuration: {error}"))?,
    )
    .map_err(|error| format!("Invalid model configuration: {error}"))?;
    if !matches!(config.model_type.as_str(), "ONNX1" | "ONNX2") {
        return Err(format!(
            "Unsupported speech model type: {}",
            config.model_type
        ));
    }
    let model_path = fetch(&config.model_file, 2)?;
    let voices_path = fetch(&config.voices, 3)?;
    progress(LoadProgress::Loading);
    kittentts::model::KittenTtsOnnx::load(
        &model_path,
        &voices_path,
        config.speed_priors,
        config.voice_aliases,
    )
    .map_err(|error| format!("Cannot initialize local speech model: {error:#}"))
}

pub(crate) fn test_speech() -> Result<(), String> {
    let mut tts = LocalTts::new();
    tts.speak("Hello! Chatty's local speech is working.", None)?;
    let started = std::time::Instant::now();
    let mut last_status = String::new();
    loop {
        if let Some(error) = tts.take_error() {
            return Err(error);
        }
        let status = tts.status();
        if status != last_status {
            println!("{status}");
            last_status = status;
        }
        if !tts.is_speaking() {
            // Errors and completion are published together by the worker.
            if let Some(error) = tts.take_error() {
                return Err(error);
            }
            println!("Local speech synthesis and playback completed.");
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(120) {
            tts.stop();
            return Err("Local speech test timed out after 120 seconds.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn voice_for(character_id: Option<&str>) -> &'static str {
    let hash = character_id
        .unwrap_or_default()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
        });
    VOICES[hash as usize % VOICES.len()]
}

fn speech_text(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            line.trim_start_matches(|character: char| {
                character.is_whitespace() || matches!(character, '#' | '>' | '-' | '*' | '`')
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
        .replace("**", "")
        .replace("__", "")
        .replace('`', "")
        .trim()
        .to_owned()
}
