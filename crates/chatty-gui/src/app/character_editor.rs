//! Character editor actions: images, imports, exports, and portrait jobs.

use crate::app::{ChattyApp, EmotionBatch, ImageJob, Message};
use crate::characters;
use crate::characters::{CharacterMessage, DraftCharacter, ImportedImageBatch};
use crate::network::Command;
use chatty_protocol::{Character, CharacterImage, CharacterInput, EntityKind, Request};
use iced::Task;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

/// Maximum concurrent `GetCharacterImage` requests to avoid overwhelming the broker.
const MAX_IN_FLIGHT_CHARACTER_IMAGES: usize = 2;

impl ChattyApp {
    pub(super) fn update_character(&mut self, msg: CharacterMessage) -> Task<Message> {
        match msg {
            CharacterMessage::CloseDialog => {
                self.draft_character_open = false;
            }
            CharacterMessage::Select(id) => {
                // Results must not land in a different character's draft.
                self.cancel_portrait_generation();
                self.enqueue_missing_character_images();
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
                for image in &self.draft.images {
                    if !image.data.is_empty() {
                        self.pending_saved_images
                            .insert(image.id.clone(), image.data.clone());
                    }
                }
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
                self.open_chat_dialog(vec![id]);
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
            CharacterMessage::Export => return self.export_character(),
            CharacterMessage::ExportFinished(error) => {
                self.file_dialog_open = false;
                if let Some(error) = error {
                    self.set_error(error);
                }
            }
            CharacterMessage::Import => return self.import_character(),
            CharacterMessage::ImportLoaded(outcome) => {
                self.file_dialog_open = false;
                if self.draft_character_open
                    && !self.token.is_empty()
                    && let Some(result) = outcome
                {
                    match result {
                        Ok(draft) => {
                            self.cancel_portrait_generation();
                            self.set_draft(*draft);
                            self.draft_world_ids.clear();
                        }
                        Err(error) => self.set_error(error),
                    }
                }
            }
            CharacterMessage::AiImport => return self.ai_import_character(),
            CharacterMessage::AiImportLoaded(outcome) => {
                self.file_dialog_open = false;
                if self.draft_character_open
                    && !self.token.is_empty()
                    && let Some(result) = outcome
                {
                    match result {
                        Ok((source_name, text)) => {
                            self.char_import_pending = true;
                            self.char_import_started = Some(Instant::now());
                            self.send(Request::ImportCharacterFromText {
                                session_token: self.token.clone(),
                                source_name,
                                text,
                            });
                        }
                        Err(error) => self.set_error(error),
                    }
                }
            }
            CharacterMessage::AddImages => return self.add_character_images(),
            CharacterMessage::AddImageFolder => return self.add_character_image_folder(),
            CharacterMessage::ImagesLoaded { epoch, batch } => {
                self.file_dialog_open = false;
                if self.draft_character_open
                    && !self.token.is_empty()
                    && epoch == self.draft_epoch
                    && let Some(result) = batch
                {
                    match result {
                        Ok(batch) => self.apply_imported_image_batch(batch),
                        Err(error) => self.set_error(error),
                    }
                }
            }
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
    /// already held for unchanged images and fetch the rest with bounded concurrency.
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
                let saved = self.pending_saved_images.remove(&image.id);
                if image.data.is_empty() {
                    match saved.or_else(|| known.remove(&key)) {
                        Some(data) => image.data = data,
                        None => {
                            wanted.insert(key);
                        }
                    }
                }
            }
        }
        self.characters = characters;
        // Forget queued requests for images that no longer exist; keep in-flight ones.
        self.character_image_requests
            .retain(|key| wanted.contains(key));
        self.character_image_queue
            .retain(|key| wanted.contains(key));
        self.enqueue_character_images_for(&wanted);
    }

    /// Re-queues any character images that still lack bytes after an offline/idle gap.
    pub(super) fn enqueue_missing_character_images(&mut self) {
        let wanted: HashSet<(String, String)> = self
            .characters
            .iter()
            .flat_map(|character| {
                character
                    .images
                    .iter()
                    .filter(|image| image.data.is_empty())
                    .map(move |image| (character.id.clone(), image.id.clone()))
            })
            .collect();
        if !wanted.is_empty() {
            self.enqueue_character_images_for(&wanted);
        }
    }

    fn enqueue_character_images_for(&mut self, wanted: &HashSet<(String, String)>) {
        // Prioritize default portraits first so roster avatars and active speakers load fastest.
        let mut ordered_wanted = Vec::with_capacity(wanted.len());
        for character in &self.characters {
            let default_id = character
                .default_image_id
                .as_deref()
                .filter(|id| character.images.iter().any(|image| image.id == *id))
                .or_else(|| character.images.first().map(|image| image.id.as_str()));
            if let Some(default_id) = default_id {
                let key = (character.id.clone(), default_id.to_owned());
                if wanted.contains(&key) {
                    ordered_wanted.push(key);
                }
            }
        }
        for character in &self.characters {
            for image in &character.images {
                let key = (character.id.clone(), image.id.clone());
                if wanted.contains(&key) {
                    ordered_wanted.push(key);
                }
            }
        }
        for key in ordered_wanted {
            if self.character_image_requests.insert(key.clone()) {
                self.character_image_queue.push_back(key);
            }
        }
        self.pump_character_image_requests();
    }

    pub(super) fn pump_character_image_requests(&mut self) {
        if self.token.is_empty() {
            return;
        }
        let mut in_flight = self
            .character_image_requests
            .len()
            .saturating_sub(self.character_image_queue.len())
            .max(self.character_image_request_ids.len());
        while in_flight < MAX_IN_FLIGHT_CHARACTER_IMAGES {
            let Some((character_id, image_id)) = self.character_image_queue.pop_front() else {
                break;
            };
            if !self
                .character_image_requests
                .contains(&(character_id.clone(), image_id.clone()))
            {
                continue;
            }
            self.send(Request::GetCharacterImage {
                session_token: self.token.clone(),
                character_id,
                image_id,
            });
            in_flight += 1;
        }
    }

    /// Load a SillyTavern JSON or PNG card into a new, unsaved draft off the UI thread.
    pub(super) fn import_character(&mut self) -> Task<Message> {
        if self.file_dialog_open {
            return Task::none();
        }
        self.file_dialog_open = true;
        Task::perform(
            async move {
                let handle = rfd::AsyncFileDialog::new()
                    .add_filter("Character archive or card", &["zip", "json", "png"])
                    .pick_file()
                    .await?;
                let path = handle.path().to_path_buf();
                let result = tokio::task::spawn_blocking(move || {
                    let extension = path
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(str::to_ascii_lowercase);
                    let draft =
                        std::fs::read(&path)
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
                    draft.map(Box::new).ok_or_else(|| {
                        "Could not read that character archive or SillyTavern card.".to_owned()
                    })
                })
                .await;
                Some(match result {
                    Ok(outcome) => outcome,
                    Err(error) => Err(format!("Could not read character file: {error}")),
                })
            },
            |outcome| Message::Character(CharacterMessage::ImportLoaded(outcome)),
        )
    }

    /// Ask the broker to turn a free-form text file into a character draft off the UI thread.
    pub(super) fn ai_import_character(&mut self) -> Task<Message> {
        if self.file_dialog_open || self.char_import_pending {
            return Task::none();
        }
        self.file_dialog_open = true;
        Task::perform(
            async move {
                let handle = rfd::AsyncFileDialog::new()
                    .add_filter("Text file", &["txt", "md", "text"])
                    .pick_file()
                    .await?;
                let path = handle.path().to_path_buf();
                let result = tokio::task::spawn_blocking(move || {
                    let text = std::fs::read_to_string(&path)
                        .map_err(|error| format!("Failed to read file: {error}"))?;
                    if text.trim().is_empty() {
                        return Err("Text file is empty.".to_owned());
                    }
                    let source_name = path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Imported character")
                        .replace(['_', '-'], " ");
                    Ok((source_name, text))
                })
                .await;
                Some(match result {
                    Ok(outcome) => outcome,
                    Err(error) => Err(format!("Failed to read file: {error}")),
                })
            },
            |outcome| Message::Character(CharacterMessage::AiImportLoaded(outcome)),
        )
    }

    pub(super) fn add_character_images(&mut self) -> Task<Message> {
        if self.file_dialog_open {
            return Task::none();
        }
        self.file_dialog_open = true;
        let epoch = self.draft_epoch;
        let remaining = characters::MAX_CHARACTER_IMAGES.saturating_sub(self.draft.images.len());
        let used: usize = self.draft.images.iter().map(|image| image.data.len()).sum();
        let draft_name = self.draft.name.clone();
        Task::perform(
            async move {
                let handles = rfd::AsyncFileDialog::new()
                    .add_filter("PNG image", &["png"])
                    .pick_files()
                    .await?;
                let paths: Vec<PathBuf> = handles
                    .into_iter()
                    .map(|handle| handle.path().to_path_buf())
                    .collect();
                let result = tokio::task::spawn_blocking(move || {
                    load_character_images_from_paths(paths, None, remaining, used, &draft_name)
                })
                .await;
                Some(result.map_err(|error| format!("Could not import images: {error}")))
            },
            move |batch| Message::Character(CharacterMessage::ImagesLoaded { epoch, batch }),
        )
    }

    /// Add every PNG in a folder; an image named after the folder becomes the default.
    pub(super) fn add_character_image_folder(&mut self) -> Task<Message> {
        if self.file_dialog_open {
            return Task::none();
        }
        self.file_dialog_open = true;
        let epoch = self.draft_epoch;
        let remaining = characters::MAX_CHARACTER_IMAGES.saturating_sub(self.draft.images.len());
        let used: usize = self.draft.images.iter().map(|image| image.data.len()).sum();
        let draft_name = self.draft.name.clone();
        Task::perform(
            async move {
                let handle = rfd::AsyncFileDialog::new().pick_folder().await?;
                let folder = handle.path().to_path_buf();
                let result = tokio::task::spawn_blocking(move || {
                    let mut paths = std::fs::read_dir(&folder)
                        .map_err(|error| format!("Could not read {}: {error}", folder.display()))?
                        .filter_map(|entry| entry.ok())
                        .map(|entry| entry.path())
                        .filter(|path| {
                            path.extension()
                                .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
                        })
                        .collect::<Vec<_>>();
                    paths.sort();
                    let folder_name = folder
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned);
                    Ok(load_character_images_from_paths(
                        paths,
                        folder_name.as_deref(),
                        remaining,
                        used,
                        &draft_name,
                    ))
                })
                .await;
                Some(match result {
                    Ok(outcome) => outcome,
                    Err(error) => Err(format!("Could not import folder: {error}")),
                })
            },
            move |batch| Message::Character(CharacterMessage::ImagesLoaded { epoch, batch }),
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn import_character_image_paths(
        &mut self,
        paths: Vec<PathBuf>,
        default_stem: Option<&str>,
    ) {
        let remaining = characters::MAX_CHARACTER_IMAGES.saturating_sub(self.draft.images.len());
        let used: usize = self.draft.images.iter().map(|image| image.data.len()).sum();
        let batch = load_character_images_from_paths(
            paths,
            default_stem,
            remaining,
            used,
            &self.draft.name,
        );
        self.apply_imported_image_batch(batch);
    }

    fn apply_imported_image_batch(&mut self, batch: ImportedImageBatch) {
        let mut notices = batch.errors;
        let mut truncated = batch.truncated;
        let mut used: usize = self.draft.images.iter().map(|image| image.data.len()).sum();
        let mut imported_default = None;
        for image in batch.images {
            if self.draft.images.len() >= characters::MAX_CHARACTER_IMAGES {
                truncated = true;
                break;
            }
            if used + image.data.len() > characters::MAX_CHARACTER_IMAGE_BYTES {
                notices.push(format!(
                    "{} was not added: this character's images would exceed 5 MiB.",
                    image.label
                ));
                continue;
            }
            used += image.data.len();
            if batch.default_image_id.as_deref() == Some(image.id.as_str()) {
                imported_default = Some(image.id.clone());
            }
            if self.draft.default_image_id.is_none() {
                self.draft.default_image_id = Some(image.id.clone());
            }
            self.draft.images.push(image);
        }
        if let Some(id) = imported_default {
            self.draft.default_image_id = Some(id);
        }
        if truncated {
            notices.push(format!(
                "Only the first {} character images were added.",
                characters::MAX_CHARACTER_IMAGES
            ));
        }
        if !notices.is_empty() {
            const MAX_SHOWN: usize = 6;
            let extra = notices.len().saturating_sub(MAX_SHOWN);
            let mut summary = notices
                .into_iter()
                .take(MAX_SHOWN)
                .collect::<Vec<_>>()
                .join("\n");
            if extra > 0 {
                summary.push_str(&format!("\n…and {extra} more image(s) skipped."));
            }
            self.set_error(summary);
        }
    }

    /// Save the draft and all its images as a zip archive the user picks off the UI thread.
    pub(super) fn export_character(&mut self) -> Task<Message> {
        if self.file_dialog_open {
            return Task::none();
        }
        if self.draft.images.iter().any(|image| image.data.is_empty()) {
            self.set_error(
                "Character images are still downloading. Try exporting again in a moment.",
            );
            return Task::none();
        }
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
        let draft = self.draft.clone();
        self.file_dialog_open = true;
        Task::perform(
            async move {
                let handle = rfd::AsyncFileDialog::new()
                    .set_file_name(format!("{name}.zip"))
                    .add_filter("Character archive", &["zip"])
                    .save_file()
                    .await?;
                let path = handle.path().to_path_buf();
                let result = tokio::task::spawn_blocking(move || {
                    let bytes = characters::export_card_zip(&draft).map_err(|error| {
                        format!("Could not build the character archive: {error}")
                    })?;
                    std::fs::write(&path, bytes)
                        .map_err(|error| format!("Could not write {}: {error}", path.display()))
                })
                .await;
                match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(error) => Some(format!("Could not export character: {error}")),
                }
            },
            |error| Message::Character(CharacterMessage::ExportFinished(error)),
        )
    }

    /// Replace the character draft and its multi-line editor buffers together.
    pub(super) fn set_draft(&mut self, draft: DraftCharacter) {
        self.draft_epoch += 1;
        self.draft_editors.load(&draft);
        self.draft = draft;
    }

    pub(super) fn store_character_image(
        &mut self,
        character_id: &str,
        image_id: &str,
        data: Vec<u8>,
    ) {
        let key = (character_id.to_owned(), image_id.to_owned());
        self.character_image_requests.remove(&key);
        self.character_image_request_ids
            .retain(|_, pending| pending != &key);
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
        self.pump_character_image_requests();
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

fn load_character_images_from_paths(
    paths: Vec<PathBuf>,
    default_stem: Option<&str>,
    remaining: usize,
    mut used: usize,
    draft_name: &str,
) -> ImportedImageBatch {
    let truncated = paths.len() > remaining;
    let draft_name = draft_name.trim();
    let mut images = Vec::new();
    let mut errors = Vec::new();
    let mut default_image_id = None;
    for path in paths.into_iter().take(remaining) {
        let Ok(source) = std::fs::read(&path) else {
            errors.push(format!("Could not read {}.", path.display()));
            continue;
        };
        if source.len() > 2 * 1024 * 1024 {
            errors.push(format!(
                "{} must be a PNG no larger than 2 MB.",
                path.display()
            ));
            continue;
        }
        let data = match characters::normalize_character_image_png(&source) {
            Ok(data) => data,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        if used + data.len() > characters::MAX_CHARACTER_IMAGE_BYTES {
            errors.push(format!(
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
            || (!draft_name.is_empty() && label.eq_ignore_ascii_case(draft_name))
        {
            default_image_id = Some(id.clone());
            label = "default".into();
        }
        images.push(CharacterImage { id, label, data });
    }
    ImportedImageBatch {
        images,
        default_image_id,
        errors,
        truncated,
    }
}
