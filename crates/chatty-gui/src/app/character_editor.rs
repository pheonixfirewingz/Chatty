//! Character editor actions: images, imports, exports, and portrait jobs.

use crate::app::{ChattyApp, EmotionBatch, ImageJob, Message};
use crate::characters;
use crate::characters::{CharacterMessage, DraftCharacter};
use crate::network::Command;
use chatty_protocol::{Character, CharacterImage, CharacterInput, EntityKind, Request};
use iced::Task;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

impl ChattyApp {
    pub(super) fn update_character(&mut self, msg: CharacterMessage) -> Task<Message> {
        match msg {
            CharacterMessage::CloseDialog => {
                self.draft_character_open = false;
            }
            CharacterMessage::Select(id) => {
                // Results must not land in a different character's draft.
                self.cancel_portrait_generation();
                if let Some(c) = self.characters.iter().find(|c| c.id == id) {
                    let draft = DraftCharacter::from(c);
                    self.set_draft(draft);
                    self.draft_world_ids = self
                        .worlds
                        .iter()
                        .filter(|w| w.character_ids.contains(&id))
                        .map(|w| w.id.clone())
                        .collect();
                }
            }
            CharacterMessage::New => {
                self.cancel_portrait_generation();
                self.set_draft(DraftCharacter::default());
                self.draft_world_ids.clear();
            }
            CharacterMessage::Save if self.portrait_image.pending => {}
            CharacterMessage::Save => {
                let input = CharacterInput::from(&self.draft);
                self.send(Request::UpsertCharacter {
                    session_token: self.token.clone(),
                    character: Box::new(input),
                    world_ids: self.draft_world_ids.iter().cloned().collect(),
                });
                self.draft_character_open = false;
            }
            CharacterMessage::Delete => {
                if let Some(id) = self.draft.id.clone() {
                    self.send(Request::DeleteEntity {
                        session_token: self.token.clone(),
                        kind: EntityKind::Character,
                        entity_id: id,
                    });
                    self.draft_character_open = false;
                }
            }
            CharacterMessage::ToggleWorldLink { world_id, linked } => {
                if linked {
                    self.draft_world_ids.insert(world_id);
                } else {
                    self.draft_world_ids.remove(&world_id);
                }
            }
            CharacterMessage::SearchChanged(query) => {
                self.character_search = query;
            }
            CharacterMessage::Chat(id) => {
                self.draft_character_open = false;
                self.selected_characters.clear();
                self.selected_characters.insert(id);
                self.new_chat_open = true;
            }
            CharacterMessage::OpenMemories(id) => {
                self.draft_character_open = false;
                self.open_memory_manager(Some(id), None);
            }
            CharacterMessage::GeneratePortrait => {
                if self.image_generation && !self.portrait_image.pending {
                    self.portrait_image.pending = true;
                    self.send(Request::GenerateImage {
                        session_token: self.token.clone(),
                        prompt: characters::portrait_prompt(&self.draft, None),
                        negative_prompt: String::new(),
                        square: true,
                        seed: None,
                    });
                }
            }
            CharacterMessage::Export => self.export_character(),
            CharacterMessage::Import => self.import_character(),
            CharacterMessage::AiImport => self.ai_import_character(),
            CharacterMessage::AddImages => self.add_character_images(),
            CharacterMessage::AddImageFolder => self.add_character_image_folder(),
            CharacterMessage::GenerateEmotions => self.start_emotion_batch(),
            CharacterMessage::CancelImageGeneration => self.cancel_portrait_generation(),
            CharacterMessage::Edit(field, action) => {
                return self
                    .draft_editors
                    .perform(&mut self.draft, field, action)
                    .map(Message::Character);
            }
            other => {
                self.draft.update(&other);
            }
        }
        Task::none()
    }

    /// Replace the roster. Listings carry image metadata only, so keep bytes
    /// already held for unchanged images and fetch the rest once.
    pub(super) fn set_characters(&mut self, mut characters: Vec<Character>) {
        let mut known: HashMap<(String, String), Vec<u8>> = HashMap::new();
        for character in self.characters.drain(..) {
            for image in character.images {
                if !image.data.is_empty() {
                    known.insert((character.id.clone(), image.id), image.data);
                }
            }
        }
        // A just-saved draft holds bytes for images the old roster never had.
        if let Some(id) = &self.draft.id {
            for image in &self.draft.images {
                if !image.data.is_empty() {
                    known
                        .entry((id.clone(), image.id.clone()))
                        .or_insert_with(|| image.data.clone());
                }
            }
        }
        let mut wanted = HashSet::new();
        for character in &mut characters {
            for image in &mut character.images {
                let key = (character.id.clone(), image.id.clone());
                if image.data.is_empty() {
                    match known.remove(&key) {
                        Some(data) => image.data = data,
                        None => {
                            wanted.insert(key);
                        }
                    }
                }
            }
        }
        self.characters = characters;
        // Forget requests for images that no longer exist; keep in-flight ones.
        self.character_image_requests
            .retain(|key| wanted.contains(key));
        for (character_id, image_id) in wanted {
            if self
                .character_image_requests
                .insert((character_id.clone(), image_id.clone()))
            {
                self.send(Request::GetCharacterImage {
                    session_token: self.token.clone(),
                    character_id,
                    image_id,
                });
            }
        }
    }

    /// Load a SillyTavern JSON or PNG card into a new, unsaved draft.
    pub(super) fn import_character(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Character archive or card", &["zip", "json", "png"])
            .pick_file()
        else {
            return;
        };
        let extension = path
            .extension()
            .and_then(|x| x.to_str())
            .map(str::to_ascii_lowercase);
        let draft = std::fs::read(&path)
            .ok()
            .and_then(|bytes| match extension.as_deref() {
                Some("zip") => characters::draft_from_zip(&bytes),
                Some("png") => characters::png_card_json(&bytes)
                    .and_then(|json| serde_json::from_slice(&json).ok())
                    .and_then(characters::draft_from_card),
                _ => serde_json::from_slice(&bytes)
                    .ok()
                    .and_then(characters::draft_from_card),
            });
        match draft {
            Some(draft) => {
                self.cancel_portrait_generation();
                self.set_draft(draft);
                self.draft_world_ids.clear();
            }
            None => self.set_error("Could not read that character archive or SillyTavern card."),
        }
    }

    /// Ask the broker to turn a free-form text file into a character draft.
    pub(super) fn ai_import_character(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Text file", &["txt", "md", "text"])
            .pick_file()
        else {
            return;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => return self.set_error(format!("Failed to read file: {error}")),
        };
        if text.trim().is_empty() {
            return self.set_error("Text file is empty.");
        }
        let source_name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Imported character")
            .replace(['_', '-'], " ");
        self.char_import_pending = true;
        self.char_import_started = Some(Instant::now());
        self.send(Request::ImportCharacterFromText {
            session_token: self.token.clone(),
            source_name,
            text,
        });
    }

    pub(super) fn add_character_images(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .pick_files()
        else {
            return;
        };
        self.import_character_image_paths(paths, None);
    }

    /// Add every PNG in a folder; an image named after the folder becomes the default.
    pub(super) fn add_character_image_folder(&mut self) {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let mut paths = match std::fs::read_dir(&folder) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                return self.set_error(format!("Could not read {}: {error}", folder.display()));
            }
        };
        paths.sort();
        let folder_name = folder
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        self.import_character_image_paths(paths, folder_name.as_deref());
    }

    pub(super) fn import_character_image_paths(
        &mut self,
        paths: Vec<PathBuf>,
        default_stem: Option<&str>,
    ) {
        let remaining = characters::MAX_CHARACTER_IMAGES.saturating_sub(self.draft.images.len());
        let truncated = paths.len() > remaining;
        let mut used: usize = self.draft.images.iter().map(|image| image.data.len()).sum();
        let mut imported_default = None;
        for path in paths.into_iter().take(remaining) {
            let Ok(source) = std::fs::read(&path) else {
                self.set_error(format!("Could not read {}.", path.display()));
                continue;
            };
            if source.len() > 2 * 1024 * 1024 {
                self.set_error(format!(
                    "{} must be a PNG no larger than 2 MB.",
                    path.display()
                ));
                continue;
            }
            let data = match characters::normalize_character_image_png(&source) {
                Ok(data) => data,
                Err(error) => {
                    self.set_error(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            // The broker rejects a save over the total image budget.
            if used + data.len() > characters::MAX_CHARACTER_IMAGE_BYTES {
                self.set_error(format!(
                    "{} was not added: this character's images would exceed 5 MiB.",
                    path.display()
                ));
                continue;
            }
            used += data.len();
            let mut label = path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("portrait")
                .replace(['_', '-'], " ");
            let id = chatty_protocol::util::new_uuid();
            if default_stem.is_some_and(|stem| label.eq_ignore_ascii_case(stem))
                || (!self.draft.name.trim().is_empty()
                    && label.eq_ignore_ascii_case(self.draft.name.trim()))
            {
                imported_default = Some(id.clone());
                label = "default".into();
            }
            if self.draft.default_image_id.is_none() {
                self.draft.default_image_id = Some(id.clone());
            }
            self.draft.images.push(CharacterImage { id, label, data });
        }
        if let Some(id) = imported_default {
            self.draft.default_image_id = Some(id);
        }
        if truncated {
            self.set_error(format!(
                "Only the first {} character images were added.",
                characters::MAX_CHARACTER_IMAGES
            ));
        }
    }

    /// Save the draft and all its images as a zip archive the user picks.
    pub(super) fn export_character(&mut self) {
        if self.draft.images.iter().any(|image| image.data.is_empty()) {
            return self.set_error(
                "Character images are still downloading. Try exporting again in a moment.",
            );
        }
        let bytes = match characters::export_card_zip(&self.draft) {
            Ok(bytes) => bytes,
            Err(error) => {
                return self.set_error(format!("Could not build the character archive: {error}"));
            }
        };
        let name: String = self
            .draft
            .name
            .trim()
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == ' ' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let name = if name.is_empty() {
            "character".into()
        } else {
            name
        };
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{name}.zip"))
            .add_filter("Character archive", &["zip"])
            .save_file()
        else {
            return;
        };
        if let Err(error) = std::fs::write(&path, bytes) {
            self.set_error(format!("Could not write {}: {error}", path.display()));
        }
    }

    /// Replace the character draft and its multi-line editor buffers together.
    pub(super) fn set_draft(&mut self, draft: DraftCharacter) {
        self.draft_editors.load(&draft);
        self.draft = draft;
    }

    pub(super) fn store_character_image(
        &mut self,
        character_id: &str,
        image_id: &str,
        data: Vec<u8>,
    ) {
        self.character_image_requests
            .remove(&(character_id.to_owned(), image_id.to_owned()));
        if self.draft.id.as_deref() == Some(character_id)
            && let Some(image) = self
                .draft
                .images
                .iter_mut()
                .find(|image| image.id == image_id && image.data.is_empty())
        {
            image.data.clone_from(&data);
        }
        if let Some(image) = self
            .characters
            .iter_mut()
            .filter(|character| character.id == character_id)
            .flat_map(|character| character.images.iter_mut())
            .find(|image| image.id == image_id)
        {
            image.data = data;
        }
    }

    pub(super) fn start_emotion_batch(&mut self) {
        if !self.image_generation || self.portrait_image.pending || self.emotion_batch.is_some() {
            return;
        }
        let queue: VecDeque<_> = characters::missing_emotions(&self.draft).into();
        if queue.is_empty() {
            return;
        }
        // A random UUID is a convenient source of seed entropy.
        let uuid = chatty_protocol::util::new_uuid().replace('-', "");
        let seed = u64::from_str_radix(&uuid[..16], 16).unwrap_or_default();
        self.emotion_batch = Some(EmotionBatch {
            seed,
            current: None,
            total: queue.len(),
            queue,
            done: 0,
        });
        self.send_next_emotion();
    }

    /// Request the next queued emotion, or end the batch when none remain.
    pub(super) fn send_next_emotion(&mut self) {
        let Some(batch) = &mut self.emotion_batch else {
            return;
        };
        let Some((label, expression)) = batch.queue.pop_front() else {
            self.emotion_batch = None;
            return;
        };
        batch.current = Some(label);
        let seed = batch.seed;
        self.portrait_image.pending = true;
        self.send(Request::GenerateImage {
            session_token: self.token.clone(),
            prompt: characters::portrait_prompt(&self.draft, Some(expression)),
            negative_prompt: "multiple people, text, watermark".into(),
            square: true,
            seed: Some(seed),
        });
    }

    pub(super) fn cancel_portrait_generation(&mut self) {
        if let Some(id) = self.portrait_image.request_id {
            let _ = self.commands.send(Command::Cancel(id));
        }
        self.portrait_image = ImageJob::default();
        self.emotion_batch = None;
    }

    pub(super) fn add_generated_portrait(&mut self, png: &[u8]) {
        let label = self
            .emotion_batch
            .as_mut()
            .and_then(|batch| batch.current.take());
        let side = if label.is_some() {
            characters::EMOTION_PORTRAIT_SIDE
        } else {
            characters::PORTRAIT_SIDE
        };
        let data = match characters::generated_portrait_png(png, side) {
            Ok(data) => data,
            Err(error) => {
                self.emotion_batch = None;
                return self.set_error(error);
            }
        };
        // Stop before a save would exceed the broker's character limits.
        let used: usize = self.draft.images.iter().map(|image| image.data.len()).sum();
        if self.draft.images.len() >= characters::MAX_CHARACTER_IMAGES
            || used + data.len() > characters::MAX_CHARACTER_IMAGE_BYTES
        {
            self.emotion_batch = None;
            return self.set_error(
                "This character has reached its image limit (32 images or 5 MiB). Remove some images to generate more.",
            );
        }
        let id = chatty_protocol::util::new_uuid();
        if self.draft.default_image_id.is_none() {
            self.draft.default_image_id = Some(id.clone());
        }
        self.draft.images.push(CharacterImage {
            id,
            label: label.unwrap_or("generated portrait").into(),
            data,
        });
        if let Some(batch) = &mut self.emotion_batch {
            batch.done += 1;
            self.send_next_emotion();
        }
    }
}
