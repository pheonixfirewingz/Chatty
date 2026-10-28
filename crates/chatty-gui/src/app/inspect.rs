//! Inspection harness: FIFO control commands and demo data.

use crate::admin;
use crate::app::{ChattyApp, Message, Screen};
use chatty_protocol::{
    AdapterStatus, BrokerLog, BrokerMonitor, Character, CharacterImage, ChatMessage, Conversation,
    ConversationKind, GenerationContext, MemoryEntry, MemoryKind, MemorySource, ModelSearchResult,
    OllamaModel, OllamaRunningModel, OllamaState, Role,
};
use iced::futures::StreamExt;
use iced::futures::stream::BoxStream;
use iced::{Size, Task, window};
use std::path::PathBuf;
use std::time::Duration;

#[allow(clippy::ptr_arg)]
pub(super) fn run_fifo_stream(path: &PathBuf) -> BoxStream<'static, String> {
    let path = path.clone();
    iced::stream::channel(
        16,
        move |mut output: iced::futures::channel::mpsc::Sender<String>| async move {
            use iced::futures::sink::SinkExt;
            if !path.exists() {
                let _ = std::process::Command::new("mkfifo").arg(&path).status();
            }
            loop {
                let path_clone = path.clone();
                let line = tokio::task::spawn_blocking(move || {
                    use std::io::BufRead;
                    let Ok(file) = std::fs::File::open(&path_clone) else {
                        return None;
                    };
                    let mut reader = std::io::BufReader::new(file);
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                        Some(line)
                    } else {
                        None
                    }
                })
                .await
                .ok()
                .flatten();

                if let Some(line) = line {
                    let line = line.trim();
                    if !line.is_empty() && output.send(line.to_owned()).await.is_err() {
                        break;
                    }
                } else {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        },
    )
    .boxed()
}

impl ChattyApp {
    /// Apply one inspection command such as `resize 800 600` or `admin 2`.
    pub(super) fn inspect_control(&mut self, cmd: &str) -> Task<Message> {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let number = |index: usize| parts.get(index).and_then(|value| value.parse().ok());
        match parts.as_slice() {
            ["resize", w, h, ..] => {
                if let (Ok(w), Ok(h)) = (w.parse(), h.parse()) {
                    return window::latest()
                        .and_then(move |id| window::resize(id, Size::new(w, h)));
                }
            }
            ["zoom", factor, ..] => {
                if let Ok(factor) = factor.parse::<f32>() {
                    self.zoom = factor.clamp(0.5, 3.0);
                }
            }
            ["screenshot", path, ..] => {
                self.pending_screenshot_path = Some(PathBuf::from(path));
                return window::latest()
                    .and_then(window::screenshot)
                    .map(Message::ScreenshotCaptured);
            }
            ["navigation", state @ ..] => {
                self.compact_sidebar_open = state.first() == Some(&"open")
            }
            ["sidebar", "open", ..] => self.sidebar_visible = true,
            ["sidebar", "closed", ..] => self.sidebar_visible = false,
            ["sidebar", ..] => self.sidebar_visible = !self.sidebar_visible,
            ["tools", ..] => {
                self.screen = if self.screen == Screen::Admin {
                    Screen::Chat
                } else {
                    Screen::Admin
                };
            }
            ["admin", ..] => {
                self.screen = Screen::Admin;
                self.admin.admin_tab = number(1).unwrap_or(0);
            }
            ["advanced", state @ ..] => {
                self.admin.models.advanced_settings = state.first() == Some(&"open");
            }
            ["models", ..] => {
                self.screen = Screen::Admin;
                self.admin.admin_tab = 2;
                self.admin.models.page = number(1).unwrap_or(0);
            }
            ["settings", ..] => self.screen = Screen::Settings,
            ["connect", ..] => return self.update_message(Message::TryConnect),
            ["new_chat", ..] => self.new_chat_open = true,
            ["worlds", ..] => self.worlds_open = true,
            ["characters", ..] => self.draft_character_open = true,
            ["memories", ..] => self.memory_dialog_open = true,
            ["close_dialogs", ..] => {
                self.new_chat_open = false;
                self.worlds_open = false;
                self.draft_character_open = false;
                self.memory_dialog_open = false;
                if matches!(self.screen, Screen::Settings | Screen::Admin) {
                    self.screen = Screen::Chat;
                }
            }
            ["hover", "none", ..] => {
                self.hovered_message_id = None;
                self.hovered_conversation_id = None;
            }
            ["hover", "conv", id @ ..] => {
                self.hovered_conversation_id = id.first().map(|id| id.to_string());
            }
            ["hover", id, ..] => {
                let id = Some(id.to_string());
                if self
                    .conversations
                    .iter()
                    .any(|c| Some(&c.id) == id.as_ref())
                {
                    self.hovered_conversation_id = id;
                } else {
                    self.hovered_message_id = id;
                }
            }
            ["hover"] => self.hovered_message_id = self.messages.last().map(|m| m.id.clone()),
            ["screen", "login", ..] => {
                self.token.clear();
                self.context_usage.clear();
                self.screen = Screen::Login;
            }
            ["screen", "main", ..] => self.screen = Screen::Chat,
            ["quit", ..] => return window::latest().and_then(window::close),
            _ => {}
        }
        Task::none()
    }

    pub(super) fn load_inspection_demo(&mut self) {
        self.role = Some(Role::Admin);
        self.token = "demo-session".into();
        let pixels = image::RgbaImage::from_fn(128, 128, |x, y| {
            let dx = (x as f32) - 64.0;
            let dy = (y as f32) - 64.0;
            if dx * dx + dy * dy <= 54.0 * 54.0 {
                image::Rgba([79, 70, 229, 255])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        });
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();

        self.characters = vec![Character {
            id: "assistant".into(),
            name: "Mara".into(),
            description: "A thoughtful research assistant stationed at an alpine observatory."
                .into(),
            personality: "Curious, warm, precise, and observant.".into(),
            scenario: "Midnight review in the high library.".into(),
            system_prompt: "Adopt a grounded, literate, direct tone.".into(),
            example_dialogue: String::new(),
            appearance: "Dark braided hair, knitted wool sweater, silver wire spectacles.".into(),
            age: "28".into(),
            gender: "female".into(),
            race: "human".into(),
            misc: String::new(),
            tags: vec!["astronomy".into(), "companion".into(), "ambient".into()],
            avatar: None,
            images: vec![CharacterImage {
                id: "demo-neutral".into(),
                label: "neutral".into(),
                data: encoded.into_inner(),
            }],
            default_image_id: Some("demo-neutral".into()),
            is_public: true,
            owned_by_user: true,
            revision: 1,
        }];
        self.conversations = vec![Conversation {
            id: "demo".into(),
            title: "Night watch at the observatory".into(),
            participant_ids: vec!["assistant".into()],
            kind: ConversationKind::Direct,
            state: String::new(),
            summary: String::new(),
            revision: 1,
        }];
        self.selected_conversation = Some("demo".into());
        self.update_context_usage(GenerationContext {
            conversation_id: "demo".into(),
            prompt_tokens: 2800,
            completion_tokens: 420,
            context_tokens: 8192,
            estimated: false,
        });
        self.messages.push(ChatMessage {
            id: "m1".into(),
            author_type: "user".into(),
            author_id: None,
            content: "Tell me about this place.".into(),
            parent_id: None,
            selected_variant_id: None,
            character_image_id: None,
            created_at: String::new(),
            revision: 1,
            variants: vec![],
        });
        self.memories = vec![
            MemoryEntry {
                id: "memory-pinned".into(),
                conversation_id: None,
                character_id: Some("assistant".into()),
                kind: MemoryKind::Fact,
                content: "The user feels calmer when plans are explained one step at a time."
                    .into(),
                importance: 90,
                pinned: true,
                confidence: 1.0,
                source: MemorySource::Manual,
                source_message_ids: vec![],
                created_at: "2026-08-20 09:10:00".into(),
                updated_at: "2026-08-20 09:10:00".into(),
                revision: 2,
            },
            MemoryEntry {
                id: "memory-event".into(),
                conversation_id: Some("demo".into()),
                character_id: Some("assistant".into()),
                kind: MemoryKind::Event,
                content: "Mara and the user discovered the old observatory above the valley."
                    .into(),
                importance: 70,
                pinned: false,
                confidence: 0.88,
                source: MemorySource::Automatic,
                source_message_ids: vec!["m1".into(), "m2".into()],
                created_at: "2026-08-24 14:30:00".into(),
                updated_at: "2026-08-24 14:30:00".into(),
                revision: 3,
            },
        ];
        self.messages.push(ChatMessage {
            id: "m2".into(),
            author_type: "character".into(),
            author_id: Some("assistant".into()),
            content:
                "# The old observatory\n\nIt has watched the valley for **two hundred years**."
                    .into(),
            parent_id: Some("m1".into()),
            selected_variant_id: None,
            character_image_id: Some("demo-neutral".into()),
            created_at: String::new(),
            revision: 1,
            variants: vec![],
        });
        self.admin.broker_monitor = Some(BrokerMonitor {
            uptime_seconds: 3725,
            cpu_percent: 1.8,
            memory_used_mb: 42,
            memory_limit_mb: Some(512),
            active_connections: 2,
            adapter_status: AdapterStatus::Online,
            adapter_model_count: 1,
            adapter_latency_ms: Some(18),
            recent_errors: vec!["2026-08-24 14:32:07 UTC · Adapter request timed out".into()],
        });
        self.admin.admin_log = BrokerLog {
            content: concat!(
                "2026-08-24T14:30:00Z  INFO chatty_broker: boot log initialized\n",
                "2026-08-24T14:30:01Z  INFO chatty_broker: TLS 1.3 broker listening address=0.0.0.0:7443\n",
                "2026-08-24T14:30:02Z  INFO chatty_broker: inference backend ready models=[\"llama3.2:3b\"]\n",
                "2026-08-24T14:32:07Z  WARN chatty_broker: connection closed peer=192.0.2.8 error=request timed out\n"
            )
            .into(),
            truncated: false,
            file_size_bytes: 412,
        };
        self.admin.adapter_plugins = vec![
            "freetoken".into(),
            "ollama".into(),
            "openai-compatible".into(),
        ];
        self.admin.broker_config.adapter_url = "http://192.0.2.50:11434".into();
        self.admin.models.saved_adapter_url = Some(self.admin.broker_config.adapter_url.clone());
        self.admin.models.downloads.insert(
            "demo-roleplay:27b".into(),
            admin::ModelDownload {
                destination: self.admin.broker_config.adapter_url.clone(),
                progress: chatty_protocol::admin::ModelDownloadProgress {
                    status: "pulling layer".into(),
                    digest: Some("sha256:demo-layer".into()),
                    total: Some(20_000_000_000),
                    completed: Some(7_500_000_000),
                },
                ..Default::default()
            },
        );
        self.admin.models.downloads.insert(
            "demo-story:9b".into(),
            admin::ModelDownload {
                destination: self.admin.broker_config.adapter_url.clone(),
                progress: chatty_protocol::admin::ModelDownloadProgress {
                    status: "pulling manifest".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        self.admin.models.results = vec![ModelSearchResult {
            id: "bartowski/Llama-3.2-1B-Instruct-GGUF".into(),
            downloads: 12345,
            files: vec![
                "Llama-3.2-1B-Instruct-Q4_K_M.gguf".into(),
                "Llama-3.2-1B-Instruct-Q8_0.gguf".into(),
            ],
        }];
        self.admin.ollama_state = Some(OllamaState {
            version: "0.12.6".into(),
            models: vec![
                OllamaModel {
                    name: "llama3.2:3b".into(),
                    size: 2_018_000_000,
                    modified_at: "2026-08-23T18:30:00Z".into(),
                    family: "llama".into(),
                    parameter_size: "3.2B".into(),
                    quantization_level: "Q4_K_M".into(),
                },
                OllamaModel {
                    name: "gemma3:4b".into(),
                    size: 3_330_000_000,
                    modified_at: "2026-08-22T12:00:00Z".into(),
                    family: "gemma3".into(),
                    parameter_size: "4.3B".into(),
                    quantization_level: "Q4_K_M".into(),
                },
            ],
            running_models: vec![OllamaRunningModel {
                name: "llama3.2:3b".into(),
                size: 2_018_000_000,
                size_vram: 1_840_000_000,
                expires_at: "2026-08-24T15:05:00Z".into(),
            }],
        });
        self.markdown.update(&self.messages, &self.stream_text);
    }
}
