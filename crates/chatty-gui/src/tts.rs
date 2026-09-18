use std::sync::mpsc;

const MODEL: &str = "KittenML/kitten-tts-nano-0.8-int8";
const VOICES: [&str; 8] = [
    "Bella", "Jasper", "Luna", "Bruno", "Rosie", "Hugo", "Kiki", "Leo",
];

enum Command {
    Speak { text: String, voice: &'static str },
    Stop,
}

pub(super) struct LocalTts {
    commands: mpsc::Sender<Command>,
    speaking: bool,
}

impl LocalTts {
    pub(super) fn new() -> Self {
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("chatty-local-tts".into())
            .spawn(move || run(receiver))
            .expect("could not start the local TTS worker");
        Self {
            commands,
            speaking: false,
        }
    }

    pub(super) fn is_available(&self) -> bool {
        true
    }

    pub(super) fn engine_name(&self) -> Option<&'static str> {
        Some("KittenTTS (local neural model)")
    }

    pub(super) fn is_speaking(&mut self) -> bool {
        self.speaking
    }

    pub(super) fn speak(&mut self, text: &str, character_id: Option<&str>) -> Result<(), String> {
        let text = speech_text(text);
        if text.is_empty() {
            return Ok(());
        }
        self.commands
            .send(Command::Speak {
                text,
                voice: voice_for(character_id),
            })
            .map_err(|_| "The local text-to-speech worker stopped unexpectedly.".to_owned())?;
        self.speaking = true;
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        self.speaking = false;
    }
}

fn run(commands: mpsc::Receiver<Command>) {
    let mut stream = None;
    let mut sink: Option<rodio::Sink> = None;
    let mut model = None;
    while let Ok(command) = commands.recv() {
        match command {
            Command::Stop => {
                if let Some(current) = sink.take() {
                    current.stop();
                }
            }
            Command::Speak { text, voice } => {
                if let Some(current) = sink.take() {
                    current.stop();
                }
                if stream.is_none() {
                    let Ok(output) = rodio::OutputStreamBuilder::open_default_stream() else {
                        continue;
                    };
                    stream = Some(output);
                }
                let tts = match model.as_ref() {
                    Some(tts) => tts,
                    None => {
                        let Ok(loaded) = kittentts::download::load_from_hub(MODEL) else {
                            continue;
                        };
                        model.insert(loaded)
                    }
                };
                let Ok(samples) = tts.generate(&text, voice, 1.0, true) else {
                    continue;
                };
                let current = rodio::Sink::connect_new(stream.as_ref().unwrap().mixer());
                current.append(rodio::buffer::SamplesBuffer::new(1, 24_000, samples));
                current.play();
                sink = Some(current);
            }
        }
    }
}

fn voice_for(character_id: Option<&str>) -> &'static str {
    let hash = character_id.unwrap_or_default().bytes().fold(
        0xcbf2_9ce4_8422_2325_u64,
        |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3),
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_common_markdown_before_speaking() {
        assert_eq!(speech_text("## Hello\n- **world**"), "Hello world");
    }

    #[test]
    fn character_voice_assignment_is_stable() {
        assert_eq!(voice_for(Some("character-one")), voice_for(Some("character-one")));
        assert!(VOICES.contains(&voice_for(Some("character-one"))));
    }
}
