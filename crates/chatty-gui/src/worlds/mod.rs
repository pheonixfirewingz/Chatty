//! World lore: dialog state, SillyTavern lorebook import, and messages.

#[cfg(test)]
mod tests;
mod view;

pub use view::*;

use chatty_protocol::{World, WorldFact};
use std::time::Instant;

/// User interaction messages for the World Lore manager dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum WorldMessage {
    /// Select a world from the picker (None = "New world", Some(id) = world by id).
    SelectWorld(Option<String>),
    /// Create a new blank world.
    NewWorld,
    /// Validate and save the current world draft.
    SaveWorld,
    /// Delete the currently selected world.
    DeleteWorld,
    /// World name changed.
    NameChanged(String),
    /// Character link toggled.
    ToggleCharacterLink { character_id: String, linked: bool },
    /// Add a new lore entry.
    AddEntry,
    /// Remove lore entry at the given index.
    RemoveEntry(usize),
    /// Search query for filtering lore entries changed.
    SearchChanged(String),
    /// Clear the entry search filter.
    ClearSearch,
    /// Lore entry enabled toggled.
    EntryEnabledToggled { index: usize, enabled: bool },
    /// Lore entry common knowledge toggled.
    EntryCommonKnowledgeToggled {
        index: usize,
        common_knowledge: bool,
    },
    /// Lore entry priority changed.
    EntryPriorityChanged { index: usize, priority: i32 },
    /// Lore entry title changed.
    EntryTitleChanged { index: usize, title: String },
    /// Lore entry keywords changed.
    EntryKeywordsChanged { index: usize, keywords: String },
    /// Lore entry content changed.
    EntryContentChanged { index: usize, content: String },
    /// Open file dialog and import a SillyTavern lorebook.
    ImportLorebook,
    /// Close the world lore dialog.
    Close,
}

/// Outgoing actions resulting from handling a `WorldMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldAction {
    None,
    Save(World),
    Delete(String),
    Import {
        source_name: String,
        lorebook_json: String,
    },
    Close,
    Error(String),
}

/// Standalone state for world dialog management.
#[derive(Debug, Clone, Default)]
pub struct WorldDialogState {
    pub world_draft: World,
    pub entry_search: String,
    pub import_pending: bool,
    pub import_started: Option<Instant>,
    pub import_notice: Option<String>,
}

impl WorldDialogState {
    /// Updates dialog state given a `WorldMessage` and available worlds.
    pub fn update(&mut self, worlds: &[World], message: WorldMessage) -> WorldAction {
        match message {
            WorldMessage::SelectWorld(None) => {
                self.world_draft = World::default();
                WorldAction::None
            }
            WorldMessage::SelectWorld(Some(id)) => {
                if let Some(found) = worlds.iter().find(|w| w.id == id) {
                    self.world_draft = found.clone();
                }
                WorldAction::None
            }
            WorldMessage::NewWorld => {
                self.world_draft = World::default();
                WorldAction::None
            }
            WorldMessage::SaveWorld => match self.world_draft.validate() {
                Err(err) => WorldAction::Error(err.to_string()),
                Ok(()) => {
                    if self.world_draft.id.is_empty() {
                        self.world_draft.id = chatty_protocol::util::new_uuid();
                    }
                    WorldAction::Save(self.world_draft.clone())
                }
            },
            WorldMessage::DeleteWorld => {
                if !self.world_draft.id.is_empty() {
                    let id = self.world_draft.id.clone();
                    self.world_draft = World::default();
                    WorldAction::Delete(id)
                } else {
                    WorldAction::None
                }
            }
            WorldMessage::NameChanged(name) => {
                self.world_draft.name = name;
                WorldAction::None
            }
            WorldMessage::ToggleCharacterLink {
                character_id,
                linked,
            } => {
                self.world_draft
                    .character_ids
                    .retain(|id| id != &character_id);
                if linked {
                    self.world_draft.character_ids.push(character_id);
                }
                WorldAction::None
            }
            WorldMessage::AddEntry => {
                self.world_draft.entries.push(WorldFact {
                    enabled: true,
                    ..Default::default()
                });
                WorldAction::None
            }
            WorldMessage::RemoveEntry(index) => {
                if index < self.world_draft.entries.len() {
                    self.world_draft.entries.remove(index);
                }
                WorldAction::None
            }
            WorldMessage::SearchChanged(query) => {
                self.entry_search = query;
                WorldAction::None
            }
            WorldMessage::ClearSearch => {
                self.entry_search.clear();
                WorldAction::None
            }
            WorldMessage::EntryEnabledToggled { index, enabled } => {
                if let Some(fact) = self.world_draft.entries.get_mut(index) {
                    fact.enabled = enabled;
                }
                WorldAction::None
            }
            WorldMessage::EntryCommonKnowledgeToggled {
                index,
                common_knowledge,
            } => {
                if let Some(fact) = self.world_draft.entries.get_mut(index) {
                    fact.common_knowledge = common_knowledge;
                }
                WorldAction::None
            }
            WorldMessage::EntryPriorityChanged { index, priority } => {
                if let Some(fact) = self.world_draft.entries.get_mut(index) {
                    fact.priority = priority;
                }
                WorldAction::None
            }
            WorldMessage::EntryTitleChanged { index, title } => {
                if let Some(fact) = self.world_draft.entries.get_mut(index) {
                    fact.title = title;
                }
                WorldAction::None
            }
            WorldMessage::EntryKeywordsChanged { index, keywords } => {
                if let Some(fact) = self.world_draft.entries.get_mut(index) {
                    fact.keywords = keywords
                        .split(',')
                        .map(str::trim)
                        .filter(|k| !k.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                WorldAction::None
            }
            WorldMessage::EntryContentChanged { index, content } => {
                if let Some(fact) = self.world_draft.entries.get_mut(index) {
                    fact.content = content;
                }
                WorldAction::None
            }
            WorldMessage::ImportLorebook => {
                if let Some((source_name, lorebook_json)) = pick_silly_tavern_lorebook() {
                    self.import_pending = true;
                    self.import_started = Some(Instant::now());
                    self.import_notice = None;
                    WorldAction::Import {
                        source_name,
                        lorebook_json,
                    }
                } else {
                    WorldAction::None
                }
            }
            WorldMessage::Close => WorldAction::Close,
        }
    }
}

/// Parses SillyTavern lorebook JSON bytes.
pub fn parse_silly_tavern_lorebook(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("SillyTavern lorebook exceeds 2 MiB".into());
    }
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| "Lorebook is not valid UTF-8 JSON".to_string())?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|error| format!("Lorebook is not valid JSON: {error}"))?;
    Ok(text)
}

/// Prompts user to pick a SillyTavern JSON lorebook file.
pub fn pick_silly_tavern_lorebook() -> Option<(String, String)> {
    let path = rfd::FileDialog::new()
        .add_filter("SillyTavern lorebook", &["json"])
        .pick_file()?;
    let bytes = std::fs::read(&path).ok()?;
    let json = parse_silly_tavern_lorebook(&bytes).ok()?;
    let source_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Imported world")
        .replace(['_', '-'], " ");
    Some((source_name, json))
}

/// Filters entries based on title, content, or keywords.
pub fn filter_entries<'a>(entries: &'a [WorldFact], query: &str) -> Vec<(usize, &'a WorldFact)> {
    let q = query.trim().to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, fact)| {
            q.is_empty()
                || fact.title.to_lowercase().contains(&q)
                || fact.content.to_lowercase().contains(&q)
                || fact.keywords.iter().any(|k| k.to_lowercase().contains(&q))
        })
        .collect()
}
