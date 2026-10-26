//! Worlds management view and messages for Iced 0.14.
#![allow(dead_code)]

use std::time::Instant;

use chatty_protocol::{Character, World, WorldFact};
use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_input, Space,
};
use iced::{Alignment, Element, Length, Padding};

use crate::ui;

// -----------------------------------------------------------------------------
// Messages
// -----------------------------------------------------------------------------

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
    ToggleCharacterLink {
        character_id: String,
        linked: bool,
    },
    /// Add a new lore entry.
    AddEntry,
    /// Remove lore entry at the given index.
    RemoveEntry(usize),
    /// Search query for filtering lore entries changed.
    SearchChanged(String),
    /// Clear the entry search filter.
    ClearSearch,
    /// Lore entry enabled toggled.
    EntryEnabledToggled {
        index: usize,
        enabled: bool,
    },
    /// Lore entry common knowledge toggled.
    EntryCommonKnowledgeToggled {
        index: usize,
        common_knowledge: bool,
    },
    /// Lore entry priority changed.
    EntryPriorityChanged {
        index: usize,
        priority: i32,
    },
    /// Lore entry title changed.
    EntryTitleChanged {
        index: usize,
        title: String,
    },
    /// Lore entry keywords changed.
    EntryKeywordsChanged {
        index: usize,
        keywords: String,
    },
    /// Lore entry content changed.
    EntryContentChanged {
        index: usize,
        content: String,
    },
    /// Open file dialog and import a SillyTavern lorebook.
    ImportLorebook,
    /// SillyTavern lorebook imported with JSON payload.
    LorebookImported {
        source_name: String,
        lorebook_json: String,
    },
    /// Close the world lore dialog.
    Close,
}

// -----------------------------------------------------------------------------
// State & Actions
// -----------------------------------------------------------------------------

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
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_world(world: World) -> Self {
        Self {
            world_draft: world,
            ..Default::default()
        }
    }

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
                self.world_draft.character_ids.retain(|id| id != &character_id);
                if linked {
                    self.world_draft.character_ids.push(character_id);
                }
                WorldAction::None
            }
            WorldMessage::AddEntry => {
                self.world_draft
                    .entries
                    .push(WorldFact { enabled: true, ..Default::default() });
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
            WorldMessage::LorebookImported {
                source_name,
                lorebook_json,
            } => {
                self.import_pending = true;
                self.import_started = Some(Instant::now());
                self.import_notice = None;
                WorldAction::Import {
                    source_name,
                    lorebook_json,
                }
            }
            WorldMessage::Close => WorldAction::Close,
        }
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

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
pub fn filter_entries<'a>(
    entries: &'a [WorldFact],
    query: &str,
) -> Vec<(usize, &'a WorldFact)> {
    let q = query.trim().to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, fact)| {
            q.is_empty()
                || fact.title.to_lowercase().contains(&q)
                || fact.content.to_lowercase().contains(&q)
                || fact
                    .keywords
                    .iter()
                    .any(|k| k.to_lowercase().contains(&q))
        })
        .collect()
}

/// Option item for the world dropdown picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldOption {
    pub id: Option<String>,
    pub label: String,
}

impl std::fmt::Display for WorldOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

// -----------------------------------------------------------------------------
// View State & Renderer
// -----------------------------------------------------------------------------

/// State passed to `world_dialog_view`.
#[derive(Debug, Clone)]
pub struct WorldViewState<'a> {
    pub worlds: &'a [World],
    pub world_draft: &'a World,
    pub characters: &'a [Character],
    pub entry_search: &'a str,
    pub import_pending: bool,
    pub import_elapsed_secs: u64,
    pub import_notice: Option<&'a str>,
    pub light_mode: bool,
    pub glass_mode: bool,
}

impl<'a> WorldViewState<'a> {
    pub fn new(
        worlds: &'a [World],
        world_draft: &'a World,
        characters: &'a [Character],
        entry_search: &'a str,
        light_mode: bool,
        glass_mode: bool,
    ) -> Self {
        Self {
            worlds,
            world_draft,
            characters,
            entry_search,
            import_pending: false,
            import_elapsed_secs: 0,
            import_notice: None,
            light_mode,
            glass_mode,
        }
    }
}

/// Renders the Iced 0.14 World Lore dialog.
pub fn world_dialog_view<'a, Message>(
    state: &WorldViewState<'a>,
) -> Element<'a, Message>
where
    Message: 'a + Clone + From<WorldMessage>,
{
    // 1. Header row
    let header = row![
        text("World lore").size(18),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding(Padding::from([2, 8]))
            .on_press(Message::from(WorldMessage::Close)),
    ]
    .align_y(Alignment::Center);

    // 2. World picker and CRUD toolbar
    let mut world_options: Vec<WorldOption> = vec![WorldOption {
        id: None,
        label: "New world".to_string(),
    }];
    for world in state.worlds {
        world_options.push(WorldOption {
            id: Some(world.id.clone()),
            label: world.name.clone(),
        });
    }

    let selected_option = if state.world_draft.id.is_empty() {
        Some(WorldOption {
            id: None,
            label: "New world".to_string(),
        })
    } else {
        Some(WorldOption {
            id: Some(state.world_draft.id.clone()),
            label: state.world_draft.name.clone(),
        })
    };

    let world_picker = pick_list(
        world_options,
        selected_option,
        |opt: WorldOption| Message::from(WorldMessage::SelectWorld(opt.id)),
    );

    let new_world_btn = button(text("New world"))
        .on_press(Message::from(WorldMessage::NewWorld));

    let import_btn = if state.import_pending {
        button(text("Importing…"))
    } else {
        button(text("Import SillyTavern"))
            .on_press(Message::from(WorldMessage::ImportLorebook))
    };

    let save_btn = button(text("Save world"))
        .on_press(Message::from(WorldMessage::SaveWorld));

    let mut toolbar = row![
        world_picker,
        new_world_btn,
        import_btn,
        save_btn,
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if !state.world_draft.id.is_empty() {
        let delete_btn = button(text("Delete world"))
            .on_press(Message::from(WorldMessage::DeleteWorld));
        toolbar = toolbar.push(delete_btn);
    }

    // 3. Status indicator
    let is_saved = state.worlds.iter().any(|w| w == state.world_draft);
    let status_label = if is_saved {
        text("Saved").size(12)
    } else {
        text("Unsaved changes — save to apply").size(12)
    };

    let mut status_column = column![status_label].spacing(4);

    if state.import_pending {
        status_column = status_column.push(
            text(format!(
                "Converting lore with AI… {}s elapsed",
                state.import_elapsed_secs
            ))
            .size(13),
        );
        status_column = status_column.push(
            text("The model is reading, classifying, and formatting every entry. Large lorebooks or a cold model can take several minutes.")
                .size(11),
        );
    } else if let Some(notice) = state.import_notice {
        status_column = status_column.push(text(notice).size(13));
    }

    // 4. World details: name and character links
    let name_label = text("World name").size(13);
    let name_input = text_input("World name", &state.world_draft.name)
        .on_input(|s| Message::from(WorldMessage::NameChanged(s)));

    let characters_label = text("Linked characters").size(13);
    let characters_sub = text("This world is available when a linked character speaks in your chats.").size(11);

    let mut char_links_col = column![].spacing(6);
    for character in state.characters {
        let linked = state.world_draft.character_ids.contains(&character.id);
        let char_id = character.id.clone();
        let toggle = ui::toggle_switch(
            linked,
            Some(character.name.as_str()),
            move |val| Message::from(WorldMessage::ToggleCharacterLink {
                character_id: char_id.clone(),
                linked: val,
            }),
        );
        char_links_col = char_links_col.push(toggle);
    }

    // 5. Lore entries section
    let lore_intro = text("Common knowledge is selected first. Other facts activate from keywords in the last six messages. Priority decides what fits within the context budget.").size(11);

    let add_entry_btn = button(text("Add lore entry"))
        .on_press(Message::from(WorldMessage::AddEntry));

    let search_input = text_input("Search entries (title, content, or keyword)", state.entry_search)
        .on_input(|s| Message::from(WorldMessage::SearchChanged(s)))
        .width(Length::Fixed(260.0));

    let mut entry_toolbar = row![add_entry_btn, search_input].spacing(8).align_y(Alignment::Center);
    if !state.entry_search.is_empty() {
        entry_toolbar = entry_toolbar.push(
            button(text("Clear")).on_press(Message::from(WorldMessage::ClearSearch)),
        );
    }

    let matching = filter_entries(&state.world_draft.entries, state.entry_search);
    let mut entries_col = column![].spacing(8);

    if !state.entry_search.is_empty() {
        entries_col = entries_col.push(
            text(format!(
                "{} of {} entries shown",
                matching.len(),
                state.world_draft.entries.len()
            ))
            .size(12),
        );
    }

    for (index, fact) in matching {
        let enabled_toggle = ui::toggle_switch(
            fact.enabled,
            Some("Enabled"),
            move |val| Message::from(WorldMessage::EntryEnabledToggled { index, enabled: val }),
        );

        let common_knowledge_toggle = ui::toggle_switch(
            fact.common_knowledge,
            Some("Common knowledge"),
            move |val| Message::from(WorldMessage::EntryCommonKnowledgeToggled {
                index,
                common_knowledge: val,
            }),
        );

        let prio_down = button(text("-").size(12))
            .padding(Padding::from([2, 6]))
            .on_press(Message::from(WorldMessage::EntryPriorityChanged {
                index,
                priority: fact.priority.saturating_sub(1),
            }));

        let prio_text = text(format!("Priority: {}", fact.priority)).size(13);

        let prio_up = button(text("+").size(12))
            .padding(Padding::from([2, 6]))
            .on_press(Message::from(WorldMessage::EntryPriorityChanged {
                index,
                priority: fact.priority.saturating_add(1),
            }));

        let remove_btn = button(text("Remove").size(12))
            .padding(Padding::from([3, 8]))
            .on_press(Message::from(WorldMessage::RemoveEntry(index)));

        let card_header = row![
            enabled_toggle,
            common_knowledge_toggle,
            prio_down,
            prio_text,
            prio_up,
            Space::new().width(Length::Fill),
            remove_btn,
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let title_input = text_input("Title", &fact.title)
            .on_input(move |t| Message::from(WorldMessage::EntryTitleChanged { index, title: t }));

        let content_input = text_input("Lore content...", &fact.content)
            .on_input(move |c| Message::from(WorldMessage::EntryContentChanged { index, content: c }));

        let mut card_col = column![card_header, title_input].spacing(6);

        if !fact.common_knowledge {
            let kw_str = fact.keywords.join(", ");
            let kw_input = text_input("Keywords (comma separated, case insensitive)", &kw_str)
                .on_input(move |k| Message::from(WorldMessage::EntryKeywordsChanged { index, keywords: k }));
            card_col = card_col.push(kw_input);
        }

        card_col = card_col.push(content_input);

        let card_widget = ui::card(card_col, state.light_mode, state.glass_mode)
            .width(Length::Fill);
        entries_col = entries_col.push(card_widget);
    }

    // Combine everything into a scrollable dialog body
    let dialog_content = column![
        header,
        toolbar.wrap(),
        status_column,
        name_label,
        name_input,
        characters_label,
        characters_sub,
        char_links_col,
        lore_intro,
        entry_toolbar.wrap(),
        entries_col,
    ]
    .spacing(10)
    .width(Length::Fill);

    let scroll = scrollable(dialog_content)
        .width(Length::Fill)
        .height(Length::Shrink);

    container(scroll)
        .padding(20)
        .width(Length::Fill)
        .max_height(720.0)
        .into()
}

// -----------------------------------------------------------------------------
// Unit Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_world_message_variants() {
        let msg = WorldMessage::SelectWorld(Some("w1".into()));
        assert_eq!(msg, WorldMessage::SelectWorld(Some("w1".into())));

        let msg_new = WorldMessage::NewWorld;
        assert_eq!(msg_new, WorldMessage::NewWorld);

        let msg_save = WorldMessage::SaveWorld;
        assert_eq!(msg_save, WorldMessage::SaveWorld);

        let msg_del = WorldMessage::DeleteWorld;
        assert_eq!(msg_del, WorldMessage::DeleteWorld);

        let msg_name = WorldMessage::NameChanged("New Name".into());
        assert_eq!(msg_name, WorldMessage::NameChanged("New Name".into()));

        let msg_link = WorldMessage::ToggleCharacterLink {
            character_id: "c1".into(),
            linked: true,
        };
        assert_eq!(
            msg_link,
            WorldMessage::ToggleCharacterLink {
                character_id: "c1".into(),
                linked: true,
            }
        );

        let msg_add = WorldMessage::AddEntry;
        assert_eq!(msg_add, WorldMessage::AddEntry);

        let msg_remove = WorldMessage::RemoveEntry(2);
        assert_eq!(msg_remove, WorldMessage::RemoveEntry(2));

        let msg_search = WorldMessage::SearchChanged("term".into());
        assert_eq!(msg_search, WorldMessage::SearchChanged("term".into()));

        let msg_clear = WorldMessage::ClearSearch;
        assert_eq!(msg_clear, WorldMessage::ClearSearch);

        let msg_enabled = WorldMessage::EntryEnabledToggled {
            index: 0,
            enabled: true,
        };
        assert_eq!(
            msg_enabled,
            WorldMessage::EntryEnabledToggled {
                index: 0,
                enabled: true,
            }
        );

        let msg_prio = WorldMessage::EntryPriorityChanged {
            index: 1,
            priority: 42,
        };
        assert_eq!(
            msg_prio,
            WorldMessage::EntryPriorityChanged {
                index: 1,
                priority: 42,
            }
        );

        let msg_import = WorldMessage::ImportLorebook;
        assert_eq!(msg_import, WorldMessage::ImportLorebook);

        let msg_close = WorldMessage::Close;
        assert_eq!(msg_close, WorldMessage::Close);
    }

    #[test]
    fn test_world_dialog_state_selection_and_crud() {
        let mut state = WorldDialogState::new();
        let existing = World {
            id: "world-1".into(),
            name: "Fantasy Realm".into(),
            character_ids: vec!["char-1".into()],
            entries: vec![WorldFact {
                title: "Magic".into(),
                content: "Everywhere".into(),
                keywords: vec!["spell".into()],
                common_knowledge: true,
                enabled: true,
                priority: 10,
            }],
        };
        let worlds = vec![existing.clone()];

        // Select world
        let action = state.update(&worlds, WorldMessage::SelectWorld(Some("world-1".into())));
        assert_eq!(action, WorldAction::None);
        assert_eq!(state.world_draft.name, "Fantasy Realm");

        // Name change
        state.update(&worlds, WorldMessage::NameChanged("Updated Realm".into()));
        assert_eq!(state.world_draft.name, "Updated Realm");

        // Toggle character
        state.update(
            &worlds,
            WorldMessage::ToggleCharacterLink {
                character_id: "char-2".into(),
                linked: true,
            },
        );
        assert!(state.world_draft.character_ids.contains(&"char-2".to_string()));

        state.update(
            &worlds,
            WorldMessage::ToggleCharacterLink {
                character_id: "char-2".into(),
                linked: false,
            },
        );
        assert!(!state.world_draft.character_ids.contains(&"char-2".to_string()));

        // Save world
        let save_action = state.update(&worlds, WorldMessage::SaveWorld);
        match save_action {
            WorldAction::Save(w) => assert_eq!(w.name, "Updated Realm"),
            _ => panic!("Expected WorldAction::Save"),
        }

        // Delete world
        let del_action = state.update(&worlds, WorldMessage::DeleteWorld);
        assert_eq!(del_action, WorldAction::Delete("world-1".into()));
        assert_eq!(state.world_draft, World::default());
    }

    #[test]
    fn test_world_entries_management() {
        let mut state = WorldDialogState::new();
        state.world_draft.name = "Test".into();

        // Add entry
        state.update(&[], WorldMessage::AddEntry);
        assert_eq!(state.world_draft.entries.len(), 1);
        assert!(state.world_draft.entries[0].enabled);

        // Edit entry fields
        state.update(
            &[],
            WorldMessage::EntryTitleChanged {
                index: 0,
                title: "Secret Cave".into(),
            },
        );
        state.update(
            &[],
            WorldMessage::EntryContentChanged {
                index: 0,
                content: "Hidden entrance behind waterfall".into(),
            },
        );
        state.update(
            &[],
            WorldMessage::EntryKeywordsChanged {
                index: 0,
                keywords: "cave, waterfall, hidden".into(),
            },
        );
        state.update(
            &[],
            WorldMessage::EntryPriorityChanged {
                index: 0,
                priority: 5,
            },
        );
        state.update(
            &[],
            WorldMessage::EntryCommonKnowledgeToggled {
                index: 0,
                common_knowledge: false,
            },
        );
        state.update(
            &[],
            WorldMessage::EntryEnabledToggled {
                index: 0,
                enabled: true,
            },
        );

        let entry = &state.world_draft.entries[0];
        assert_eq!(entry.title, "Secret Cave");
        assert_eq!(entry.content, "Hidden entrance behind waterfall");
        assert_eq!(entry.keywords, vec!["cave", "waterfall", "hidden"]);
        assert_eq!(entry.priority, 5);
        assert!(!entry.common_knowledge);
        assert!(entry.enabled);

        // Search & filter
        state.update(&[], WorldMessage::SearchChanged("waterfall".into()));
        assert_eq!(state.entry_search, "waterfall");

        let matches = filter_entries(&state.world_draft.entries, &state.entry_search);
        assert_eq!(matches.len(), 1);

        state.update(&[], WorldMessage::SearchChanged("nonexistent".into()));
        let no_matches = filter_entries(&state.world_draft.entries, &state.entry_search);
        assert_eq!(no_matches.len(), 0);

        state.update(&[], WorldMessage::ClearSearch);
        assert_eq!(state.entry_search, "");

        // Remove entry
        state.update(&[], WorldMessage::RemoveEntry(0));
        assert_eq!(state.world_draft.entries.len(), 0);
    }

    #[test]
    fn test_parse_silly_tavern_lorebook() {
        let valid_json = br#"{"entries": {"0": {"key": ["forest"], "comment": "The dark woods"}}}"#;
        let parsed = parse_silly_tavern_lorebook(valid_json);
        assert!(parsed.is_ok());

        let invalid_json = b"not a json";
        assert!(parse_silly_tavern_lorebook(invalid_json).is_err());

        let too_large = vec![b'a'; 3 * 1024 * 1024];
        assert!(parse_silly_tavern_lorebook(&too_large).is_err());
    }

    #[test]
    fn test_world_dialog_view_creation() {
        let worlds = vec![World {
            id: "w1".into(),
            name: "Kingdom".into(),
            character_ids: vec![],
            entries: vec![WorldFact {
                title: "King".into(),
                content: "Reigns wisely".into(),
                keywords: vec![],
                common_knowledge: true,
                enabled: true,
                priority: 0,
            }],
        }];
        let characters = vec![Character {
            id: "c1".into(),
            name: "Arthur".into(),
            description: String::new(),
            personality: String::new(),
            scenario: String::new(),
            system_prompt: String::new(),
            example_dialogue: String::new(),
            appearance: String::new(),
            age: String::new(),
            gender: String::new(),
            race: String::new(),
            misc: String::new(),
            tags: Vec::new(),
            avatar: None,
            images: Vec::new(),
            default_image_id: None,
            is_public: false,
            owned_by_user: true,
            revision: 0,
        }];
        let state = WorldViewState::new(
            &worlds,
            &worlds[0],
            &characters,
            "",
            false,
            false,
        );

        let _element: Element<'_, WorldMessage> = world_dialog_view(&state);
    }
}
