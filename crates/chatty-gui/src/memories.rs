//! Memories management view and messages for Iced 0.14.
#![allow(dead_code)]

use chatty_protocol::{
    Character, Conversation, MemoryEntry, MemoryInput, MemoryKind,
};
use iced::widget::{
    button, column, container, pick_list, row, scrollable, slider, text, text_input, Space,
};
use iced::{Alignment, Element, Length, Padding};

use crate::ui;

// -----------------------------------------------------------------------------
// Messages
// -----------------------------------------------------------------------------

/// User interaction messages for the Memories dialog and editor.
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryMessage {
    /// Search filter changed.
    SearchChanged(String),
    /// Clear the search filter.
    ClearSearch,
    /// Filter memories by character ID (None = All characters).
    FilterCharacter(Option<String>),
    /// Filter memories by conversation ID (None = All conversations).
    FilterConversation(Option<String>),
    /// Filter memories by memory kind (None = All kinds).
    FilterKind(Option<MemoryKind>),
    /// Open the editor to create a new memory.
    AddMemory,
    /// Request the broker extract memories from recent chat messages.
    ExtractFromChat,
    /// Refresh memory list from the broker.
    Refresh,
    /// Open editor for an existing memory entry.
    EditMemory(String),
    /// Memory content text changed in editor.
    ContentChanged(String),
    /// Memory kind changed in editor.
    KindChanged(MemoryKind),
    /// Linked character changed in editor (None = Global).
    CharacterChanged(Option<String>),
    /// Linked conversation changed in editor (None = Global).
    ConversationChanged(Option<String>),
    /// Pinned state toggled in editor.
    PinnedToggled(bool),
    /// Importance rating changed in editor (1..=10).
    ImportanceChanged(u8),
    /// Save the memory currently in the editor.
    SaveMemory,
    /// Cancel editing or adding a memory.
    CancelEdit,
    /// Request deletion of a memory entry (prompts confirmation).
    RequestDelete(String),
    /// Confirm deletion of the memory.
    ConfirmDelete(String),
    /// Cancel deletion confirmation.
    CancelDelete,
    /// Close the memory dialog.
    Close,
}

// -----------------------------------------------------------------------------
// Actions & State
// -----------------------------------------------------------------------------

/// Outgoing actions resulting from handling a `MemoryMessage`.
#[derive(Debug, Clone)]
pub enum MemoryAction {
    None,
    Save(MemoryInput),
    Delete(String),
    ExtractFromChat,
    Refresh {
        character_id: Option<String>,
        conversation_id: Option<String>,
    },
    Close,
    Error(String),
}

/// Standalone state for memory dialog management.
#[derive(Debug, Clone, Default)]
pub struct MemoryDialogState {
    pub filter_character_id: Option<String>,
    pub filter_conversation_id: Option<String>,
    pub filter_kind: Option<MemoryKind>,
    pub search_query: String,
    pub editing_memory: Option<MemoryInput>,
    pub delete_confirm_id: Option<String>,
    pub status_message: Option<String>,
}

impl MemoryDialogState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_filters(
        character_id: Option<String>,
        conversation_id: Option<String>,
    ) -> Self {
        Self {
            filter_character_id: character_id,
            filter_conversation_id: conversation_id,
            ..Default::default()
        }
    }

    /// Handles a `MemoryMessage` and returns an outgoing `MemoryAction`.
    pub fn update(
        &mut self,
        memories: &[MemoryEntry],
        message: MemoryMessage,
    ) -> MemoryAction {
        match message {
            MemoryMessage::SearchChanged(query) => {
                self.search_query = query;
                MemoryAction::None
            }
            MemoryMessage::ClearSearch => {
                self.search_query.clear();
                MemoryAction::None
            }
            MemoryMessage::FilterCharacter(character_id) => {
                self.filter_character_id = character_id.clone();
                MemoryAction::Refresh {
                    character_id,
                    conversation_id: self.filter_conversation_id.clone(),
                }
            }
            MemoryMessage::FilterConversation(conversation_id) => {
                self.filter_conversation_id = conversation_id.clone();
                MemoryAction::Refresh {
                    character_id: self.filter_character_id.clone(),
                    conversation_id,
                }
            }
            MemoryMessage::FilterKind(kind) => {
                self.filter_kind = kind;
                MemoryAction::None
            }
            MemoryMessage::AddMemory => {
                self.editing_memory = Some(blank_memory(
                    self.filter_character_id.clone(),
                    self.filter_conversation_id.clone(),
                ));
                MemoryAction::None
            }
            MemoryMessage::ExtractFromChat => MemoryAction::ExtractFromChat,
            MemoryMessage::Refresh => MemoryAction::Refresh {
                character_id: self.filter_character_id.clone(),
                conversation_id: self.filter_conversation_id.clone(),
            },
            MemoryMessage::EditMemory(id) => {
                if let Some(entry) = memories.iter().find(|m| m.id == id) {
                    self.editing_memory = Some(MemoryInput {
                        id: Some(entry.id.clone()),
                        conversation_id: entry.conversation_id.clone(),
                        character_id: entry.character_id.clone(),
                        kind: entry.kind,
                        content: entry.content.clone(),
                        importance: entry.importance,
                        pinned: entry.pinned,
                    });
                }
                MemoryAction::None
            }
            MemoryMessage::ContentChanged(content) => {
                if let Some(ref mut input) = self.editing_memory {
                    input.content = content;
                }
                MemoryAction::None
            }
            MemoryMessage::KindChanged(kind) => {
                if let Some(ref mut input) = self.editing_memory {
                    input.kind = kind;
                }
                MemoryAction::None
            }
            MemoryMessage::CharacterChanged(character_id) => {
                if let Some(ref mut input) = self.editing_memory {
                    input.character_id = character_id;
                }
                MemoryAction::None
            }
            MemoryMessage::ConversationChanged(conversation_id) => {
                if let Some(ref mut input) = self.editing_memory {
                    input.conversation_id = conversation_id;
                }
                MemoryAction::None
            }
            MemoryMessage::PinnedToggled(pinned) => {
                if let Some(ref mut input) = self.editing_memory {
                    input.pinned = pinned;
                }
                MemoryAction::None
            }
            MemoryMessage::ImportanceChanged(importance) => {
                if let Some(ref mut input) = self.editing_memory {
                    input.importance = importance.clamp(1, 10);
                }
                MemoryAction::None
            }
            MemoryMessage::SaveMemory => {
                if let Some(input) = self.editing_memory.take() {
                    if input.content.trim().is_empty() {
                        self.editing_memory = Some(input);
                        MemoryAction::Error("Memory content cannot be empty.".into())
                    } else {
                        MemoryAction::Save(input)
                    }
                } else {
                    MemoryAction::None
                }
            }
            MemoryMessage::CancelEdit => {
                self.editing_memory = None;
                MemoryAction::None
            }
            MemoryMessage::RequestDelete(id) => {
                self.delete_confirm_id = Some(id);
                MemoryAction::None
            }
            MemoryMessage::ConfirmDelete(id) => {
                self.delete_confirm_id = None;
                MemoryAction::Delete(id)
            }
            MemoryMessage::CancelDelete => {
                self.delete_confirm_id = None;
                MemoryAction::None
            }
            MemoryMessage::Close => MemoryAction::Close,
        }
    }
}

// -----------------------------------------------------------------------------
// Helpers & Filtering
// -----------------------------------------------------------------------------

/// Constructs a blank `MemoryInput` pre-populated with optional scope and default values.
pub fn blank_memory(
    character_id: Option<String>,
    conversation_id: Option<String>,
) -> MemoryInput {
    MemoryInput {
        id: None,
        conversation_id,
        character_id,
        kind: MemoryKind::Fact,
        content: String::new(),
        importance: 5,
        pinned: false,
    }
}

/// Filters memories based on character, conversation, kind, and search text.
pub fn filter_memories<'a>(
    memories: &'a [MemoryEntry],
    character_id: Option<&str>,
    conversation_id: Option<&str>,
    kind: Option<MemoryKind>,
    query: &str,
) -> Vec<&'a MemoryEntry> {
    let q = query.trim().to_lowercase();
    memories
        .iter()
        .filter(|m| {
            if let Some(cid) = character_id {
                if m.character_id.as_deref() != Some(cid) {
                    return false;
                }
            }
            if let Some(cvid) = conversation_id {
                if m.conversation_id.as_deref().is_some_and(|id| id != cvid) {
                    return false;
                }
            }
            if let Some(k) = kind {
                if m.kind != k {
                    return false;
                }
            }
            q.is_empty() || m.content.to_lowercase().contains(&q)
        })
        .collect()
}

// -----------------------------------------------------------------------------
// Pick List Option Types
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterOption {
    pub id: Option<String>,
    pub label: String,
}

impl std::fmt::Display for CharacterOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationOption {
    pub id: Option<String>,
    pub label: String,
}

impl std::fmt::Display for ConversationOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindOption {
    pub kind: Option<MemoryKind>,
    pub label: &'static str,
}

impl std::fmt::Display for KindOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryKindItem(pub MemoryKind);

impl std::fmt::Display for MemoryKindItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.label())
    }
}

// -----------------------------------------------------------------------------
// View State & Renderer
// -----------------------------------------------------------------------------

/// State passed to `memory_dialog_view`.
#[derive(Debug, Clone)]
pub struct MemoryViewState<'a> {
    pub memories: &'a [MemoryEntry],
    pub characters: &'a [Character],
    pub conversations: &'a [Conversation],
    pub state: &'a MemoryDialogState,
    pub can_extract: bool,
    pub light_mode: bool,
    pub glass_mode: bool,
}

impl<'a> MemoryViewState<'a> {
    pub fn new(
        memories: &'a [MemoryEntry],
        characters: &'a [Character],
        conversations: &'a [Conversation],
        state: &'a MemoryDialogState,
        can_extract: bool,
        light_mode: bool,
        glass_mode: bool,
    ) -> Self {
        Self {
            memories,
            characters,
            conversations,
            state,
            can_extract,
            light_mode,
            glass_mode,
        }
    }
}

/// Renders the Iced 0.14 Memories management dialog.
pub fn memory_dialog_view<'a, Message>(
    view: &MemoryViewState<'a>,
) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let state = view.state;

    // 1. Header
    let header = row![
        text("Memories").size(18),
        Space::new().width(Length::Fill),
        button(text("Refresh").size(12))
            .padding(Padding::from([3, 8]))
            .on_press(Message::from(MemoryMessage::Refresh)),
        button(text("✕").size(14))
            .padding(Padding::from([2, 8]))
            .on_press(Message::from(MemoryMessage::Close)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    // 2. Filter Toolbar
    // Character filter options
    let mut char_options = vec![CharacterOption {
        id: None,
        label: "All characters".into(),
    }];
    for c in view.characters {
        char_options.push(CharacterOption {
            id: Some(c.id.clone()),
            label: c.name.clone(),
        });
    }
    let selected_char_opt = if let Some(ref cid) = state.filter_character_id {
        char_options.iter().find(|o| o.id.as_deref() == Some(cid)).cloned()
    } else {
        Some(char_options[0].clone())
    };
    let char_picker = pick_list(
        char_options,
        selected_char_opt,
        |opt: CharacterOption| Message::from(MemoryMessage::FilterCharacter(opt.id)),
    );

    // Conversation filter options
    let mut conv_options = vec![ConversationOption {
        id: None,
        label: "All conversations".into(),
    }];
    for conv in view.conversations {
        conv_options.push(ConversationOption {
            id: Some(conv.id.clone()),
            label: conv.title.clone(),
        });
    }
    let selected_conv_opt = if let Some(ref cvid) = state.filter_conversation_id {
        conv_options.iter().find(|o| o.id.as_deref() == Some(cvid)).cloned()
    } else {
        Some(conv_options[0].clone())
    };
    let conv_picker = pick_list(
        conv_options,
        selected_conv_opt,
        |opt: ConversationOption| Message::from(MemoryMessage::FilterConversation(opt.id)),
    );

    // Kind filter options
    let kind_options = vec![
        KindOption { kind: None, label: "All kinds" },
        KindOption { kind: Some(MemoryKind::Fact), label: "Facts" },
        KindOption { kind: Some(MemoryKind::Event), label: "Events" },
        KindOption { kind: Some(MemoryKind::Relationship), label: "Relationships" },
        KindOption { kind: Some(MemoryKind::Reflection), label: "Reflections" },
    ];
    let selected_kind_opt = kind_options
        .iter()
        .find(|o| o.kind == state.filter_kind)
        .copied()
        .unwrap_or(kind_options[0]);
    let kind_picker = pick_list(
        kind_options,
        Some(selected_kind_opt),
        |opt: KindOption| Message::from(MemoryMessage::FilterKind(opt.kind)),
    );

    let search_bar = text_input("Search memories…", &state.search_query)
        .on_input(|q| Message::from(MemoryMessage::SearchChanged(q)))
        .width(Length::Fixed(180.0));

    let mut filter_row = row![
        char_picker,
        conv_picker,
        kind_picker,
        search_bar,
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if !state.search_query.is_empty() {
        filter_row = filter_row.push(
            button(text("Clear").size(12))
                .padding(Padding::from([3, 8]))
                .on_press(Message::from(MemoryMessage::ClearSearch)),
        );
    }

    let mut action_row = row![].spacing(8).align_y(Alignment::Center);
    if view.can_extract {
        action_row = action_row.push(
            button(text("Extract from chat").size(13))
                .padding(Padding::from([4, 10]))
                .on_press(Message::from(MemoryMessage::ExtractFromChat)),
        );
    }
    action_row = action_row.push(
        button(text("New memory").size(13))
            .padding(Padding::from([4, 10]))
            .on_press(Message::from(MemoryMessage::AddMemory)),
    );

    let toolbar_box = column![filter_row.wrap(), action_row.wrap()].spacing(6);

    // 3. Delete Confirmation Prompt (if active)
    let delete_confirm_widget: Option<Element<'a, Message>> = state.delete_confirm_id.as_ref().map(|id| {
        let del_id = id.clone();
        let prompt_text = text("Are you sure you want to permanently delete this memory?").size(13);
        let confirm_btn = button(text("Delete").size(12))
            .padding(Padding::from([4, 10]))
            .on_press(Message::from(MemoryMessage::ConfirmDelete(del_id)));
        let cancel_btn = button(text("Cancel").size(12))
            .padding(Padding::from([4, 10]))
            .on_press(Message::from(MemoryMessage::CancelDelete));
        let actions = row![confirm_btn, cancel_btn].spacing(8);

        ui::card(
            column![prompt_text, actions].spacing(8),
            view.light_mode,
            view.glass_mode,
        )
        .width(Length::Fill)
        .into()
    });

    // 4. Memory Editor (if active)
    let editor_widget: Option<Element<'a, Message>> = state.editing_memory.as_ref().map(|draft| {
        let title_text = if draft.id.is_some() {
            "Edit memory"
        } else {
            "Create new memory"
        };

        // Kind selector
        let kind_items = vec![
            MemoryKindItem(MemoryKind::Fact),
            MemoryKindItem(MemoryKind::Event),
            MemoryKindItem(MemoryKind::Relationship),
            MemoryKindItem(MemoryKind::Reflection),
        ];
        let current_kind_item = MemoryKindItem(draft.kind);
        let kind_picker = pick_list(
            kind_items,
            Some(current_kind_item),
            |item: MemoryKindItem| Message::from(MemoryMessage::KindChanged(item.0)),
        );

        // Character selector
        let mut edit_char_options = vec![CharacterOption {
            id: None,
            label: "Global (no character)".into(),
        }];
        for c in view.characters {
            edit_char_options.push(CharacterOption {
                id: Some(c.id.clone()),
                label: c.name.clone(),
            });
        }
        let current_char_opt = if let Some(ref cid) = draft.character_id {
            edit_char_options.iter().find(|o| o.id.as_deref() == Some(cid)).cloned()
        } else {
            Some(edit_char_options[0].clone())
        };
        let edit_char_picker = pick_list(
            edit_char_options,
            current_char_opt,
            |opt: CharacterOption| Message::from(MemoryMessage::CharacterChanged(opt.id)),
        );

        // Conversation selector
        let mut edit_conv_options = vec![ConversationOption {
            id: None,
            label: "Global (no conversation)".into(),
        }];
        for conv in view.conversations {
            edit_conv_options.push(ConversationOption {
                id: Some(conv.id.clone()),
                label: conv.title.clone(),
            });
        }
        let current_conv_opt = if let Some(ref cvid) = draft.conversation_id {
            edit_conv_options.iter().find(|o| o.id.as_deref() == Some(cvid)).cloned()
        } else {
            Some(edit_conv_options[0].clone())
        };
        let edit_conv_picker = pick_list(
            edit_conv_options,
            current_conv_opt,
            |opt: ConversationOption| Message::from(MemoryMessage::ConversationChanged(opt.id)),
        );

        // Importance Slider
        let importance_slider = slider(
            1..=10,
            draft.importance,
            |val| Message::from(MemoryMessage::ImportanceChanged(val)),
        )
        .width(Length::Fixed(140.0));
        let importance_label = text(format!("Importance: {}/10", draft.importance)).size(12);

        // Pinned Toggle
        let pinned_toggle = ui::toggle_switch(
            draft.pinned,
            Some("Pinned"),
            |val| Message::from(MemoryMessage::PinnedToggled(val)),
        );

        let selectors_row = row![
            column![text("Kind").size(11), kind_picker].spacing(2),
            column![text("Character").size(11), edit_char_picker].spacing(2),
            column![text("Conversation").size(11), edit_conv_picker].spacing(2),
            column![importance_label, importance_slider].spacing(2),
            column![text("Always include").size(11), pinned_toggle].spacing(2),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        // Content Input
        let content_input = text_input("Memory content…", &draft.content)
            .on_input(|s| Message::from(MemoryMessage::ContentChanged(s)))
            .padding(Padding::from([6, 8]))
            .width(Length::Fill);

        // Save & Cancel Buttons
        let save_btn = button(text("Save memory").size(13))
            .padding(Padding::from([4, 12]))
            .on_press(Message::from(MemoryMessage::SaveMemory));
        let cancel_btn = button(text("Cancel").size(13))
            .padding(Padding::from([4, 10]))
            .on_press(Message::from(MemoryMessage::CancelEdit));
        let button_row = row![save_btn, cancel_btn].spacing(8);

        let editor_card = ui::card(
            column![
                text(title_text).size(14),
                selectors_row.wrap(),
                content_input,
                button_row,
            ]
            .spacing(8),
            view.light_mode,
            view.glass_mode,
        )
        .width(Length::Fill);

        editor_card.into()
    });

    // 5. Memory Cards List
    let filtered = filter_memories(
        view.memories,
        state.filter_character_id.as_deref(),
        state.filter_conversation_id.as_deref(),
        state.filter_kind,
        &state.search_query,
    );

    let mut list_column = column![].spacing(8).width(Length::Fill);

    if filtered.is_empty() {
        list_column = list_column.push(
            text("No memories found matching the filters.").size(13),
        );
    } else {
        list_column = list_column.push(
            text(format!("{} memories found", filtered.len())).size(12),
        );

        for memory in filtered {
            let mid = memory.id.clone();
            let mid_del = memory.id.clone();

            let kind_badge = text(memory.kind.label()).size(12);
            let importance_badge = text(format!("★ {}", memory.importance)).size(12);
            let source_badge = text(memory.source.label()).size(11);

            let mut meta_row = row![
                kind_badge,
                importance_badge,
                source_badge,
            ]
            .spacing(10)
            .align_y(Alignment::Center);

            if memory.pinned {
                meta_row = meta_row.push(text("📌 Pinned").size(11));
            }

            if memory.confidence > 0.0 {
                meta_row = meta_row.push(
                    text(format!("{:.0}% conf", memory.confidence * 100.0)).size(11),
                );
            }

            meta_row = meta_row.push(Space::new().width(Length::Fill));

            let edit_btn = button(text("Edit").size(12))
                .padding(Padding::from([2, 8]))
                .on_press(Message::from(MemoryMessage::EditMemory(mid)));
            let delete_btn = button(text("Delete").size(12))
                .padding(Padding::from([2, 8]))
                .on_press(Message::from(MemoryMessage::RequestDelete(mid_del)));

            meta_row = meta_row.push(edit_btn);
            meta_row = meta_row.push(delete_btn);

            let content_display = text(&memory.content).size(13);

            let mut scope_parts = Vec::new();
            if let Some(ref cid) = memory.character_id {
                let char_name = view
                    .characters
                    .iter()
                    .find(|c| &c.id == cid)
                    .map(|c| c.name.as_str())
                    .unwrap_or("Unknown character");
                scope_parts.push(format!("Character: {char_name}"));
            }
            if let Some(ref cvid) = memory.conversation_id {
                let conv_title = view
                    .conversations
                    .iter()
                    .find(|cv| &cv.id == cvid)
                    .map(|cv| cv.title.as_str())
                    .unwrap_or("Unknown conversation");
                scope_parts.push(format!("Chat: {conv_title}"));
            }
            let scope_text = if scope_parts.is_empty() {
                "Scope: Global".to_string()
            } else {
                scope_parts.join(" · ")
            };
            let scope_label = text(scope_text).size(11);

            let card_inner = column![
                meta_row,
                content_display,
                scope_label,
            ]
            .spacing(6);

            let memory_card = ui::card(card_inner, view.light_mode, view.glass_mode)
                .width(Length::Fill);

            list_column = list_column.push(memory_card);
        }
    }

    // 6. Dialog Content Composition
    let mut dialog_content = column![
        header,
        toolbar_box,
    ]
    .spacing(10)
    .width(Length::Fill);

    if let Some(confirm_w) = delete_confirm_widget {
        dialog_content = dialog_content.push(confirm_w);
    }

    if let Some(editor_w) = editor_widget {
        dialog_content = dialog_content.push(editor_w);
    }

    dialog_content = dialog_content.push(list_column);

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
    use chatty_protocol::MemorySource;

    fn sample_entries() -> Vec<MemoryEntry> {
        vec![
            MemoryEntry {
                id: "m1".into(),
                conversation_id: Some("conv-1".into()),
                character_id: Some("char-1".into()),
                kind: MemoryKind::Fact,
                content: "Lyra was trained as an archer in Silverwood.".into(),
                importance: 8,
                pinned: true,
                confidence: 0.95,
                source: MemorySource::Manual,
                source_message_ids: vec![],
                created_at: "2026-09-01T10:00:00Z".into(),
                updated_at: "2026-09-01T10:00:00Z".into(),
                revision: 1,
            },
            MemoryEntry {
                id: "m2".into(),
                conversation_id: Some("conv-1".into()),
                character_id: Some("char-2".into()),
                kind: MemoryKind::Event,
                content: "The party defeated the shadow beast at the river crossing.".into(),
                importance: 6,
                pinned: false,
                confidence: 0.85,
                source: MemorySource::Automatic,
                source_message_ids: vec!["msg-1".into()],
                created_at: "2026-09-02T12:00:00Z".into(),
                updated_at: "2026-09-02T12:00:00Z".into(),
                revision: 1,
            },
            MemoryEntry {
                id: "m3".into(),
                conversation_id: None,
                character_id: Some("char-1".into()),
                kind: MemoryKind::Relationship,
                content: "Lyra respects brave adventurers.".into(),
                importance: 7,
                pinned: false,
                confidence: 1.0,
                source: MemorySource::Manual,
                source_message_ids: vec![],
                created_at: "2026-09-03T14:00:00Z".into(),
                updated_at: "2026-09-03T14:00:00Z".into(),
                revision: 1,
            },
        ]
    }

    #[test]
    fn conversation_filter_includes_cross_chat_memories_and_excludes_other_chats() {
        let mut entries = sample_entries();
        entries[1].conversation_id = Some("other-chat".into());
        let filtered = filter_memories(&entries, Some("char-1"), Some("conv-1"), None, "");
        assert_eq!(filtered.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), ["m1", "m3"]);
        let other_chat = filter_memories(&entries, Some("char-1"), Some("new-chat"), None, "");
        assert_eq!(other_chat.len(), 1);
        assert_eq!(other_chat[0].id, "m3");
    }

    #[test]
    fn test_memory_message_variants() {
        let msg_search = MemoryMessage::SearchChanged("archer".into());
        assert_eq!(msg_search, MemoryMessage::SearchChanged("archer".into()));

        let msg_clear = MemoryMessage::ClearSearch;
        assert_eq!(msg_clear, MemoryMessage::ClearSearch);

        let msg_char = MemoryMessage::FilterCharacter(Some("char-1".into()));
        assert_eq!(msg_char, MemoryMessage::FilterCharacter(Some("char-1".into())));

        let msg_conv = MemoryMessage::FilterConversation(Some("conv-1".into()));
        assert_eq!(msg_conv, MemoryMessage::FilterConversation(Some("conv-1".into())));

        let msg_kind = MemoryMessage::FilterKind(Some(MemoryKind::Fact));
        assert_eq!(msg_kind, MemoryMessage::FilterKind(Some(MemoryKind::Fact)));

        let msg_add = MemoryMessage::AddMemory;
        assert_eq!(msg_add, MemoryMessage::AddMemory);

        let msg_extract = MemoryMessage::ExtractFromChat;
        assert_eq!(msg_extract, MemoryMessage::ExtractFromChat);

        let msg_refresh = MemoryMessage::Refresh;
        assert_eq!(msg_refresh, MemoryMessage::Refresh);

        let msg_edit = MemoryMessage::EditMemory("m1".into());
        assert_eq!(msg_edit, MemoryMessage::EditMemory("m1".into()));

        let msg_content = MemoryMessage::ContentChanged("new text".into());
        assert_eq!(msg_content, MemoryMessage::ContentChanged("new text".into()));

        let msg_prio = MemoryMessage::ImportanceChanged(9);
        assert_eq!(msg_prio, MemoryMessage::ImportanceChanged(9));

        let msg_pinned = MemoryMessage::PinnedToggled(true);
        assert_eq!(msg_pinned, MemoryMessage::PinnedToggled(true));

        let msg_save = MemoryMessage::SaveMemory;
        assert_eq!(msg_save, MemoryMessage::SaveMemory);

        let msg_cancel = MemoryMessage::CancelEdit;
        assert_eq!(msg_cancel, MemoryMessage::CancelEdit);

        let msg_req_del = MemoryMessage::RequestDelete("m1".into());
        assert_eq!(msg_req_del, MemoryMessage::RequestDelete("m1".into()));

        let msg_conf_del = MemoryMessage::ConfirmDelete("m1".into());
        assert_eq!(msg_conf_del, MemoryMessage::ConfirmDelete("m1".into()));

        let msg_canc_del = MemoryMessage::CancelDelete;
        assert_eq!(msg_canc_del, MemoryMessage::CancelDelete);

        let msg_close = MemoryMessage::Close;
        assert_eq!(msg_close, MemoryMessage::Close);
    }

    #[test]
    fn test_filter_memories() {
        let entries = sample_entries();

        // No filters: all entries returned
        let all = filter_memories(&entries, None, None, None, "");
        assert_eq!(all.len(), 3);

        // Filter by character "char-1"
        let char1 = filter_memories(&entries, Some("char-1"), None, None, "");
        assert_eq!(char1.len(), 2);

        // Filter by conversation "conv-1"
        let conv1 = filter_memories(&entries, None, Some("conv-1"), None, "");
        assert_eq!(conv1.len(), 3);

        // Filter by character and conversation
        let both = filter_memories(&entries, Some("char-1"), Some("conv-1"), None, "");
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].id, "m1");

        // Filter by kind
        let facts = filter_memories(&entries, None, None, Some(MemoryKind::Fact), "");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].id, "m1");

        let events = filter_memories(&entries, None, None, Some(MemoryKind::Event), "");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, "m2");

        // Filter by search query
        let query_archer = filter_memories(&entries, None, None, None, "archer");
        assert_eq!(query_archer.len(), 1);
        assert_eq!(query_archer[0].id, "m1");

        let query_beast = filter_memories(&entries, None, None, None, "SHADOW BEAST");
        assert_eq!(query_beast.len(), 1);
        assert_eq!(query_beast[0].id, "m2");

        let query_none = filter_memories(&entries, None, None, None, "dragon");
        assert_eq!(query_none.len(), 0);
    }

    #[test]
    fn test_blank_memory() {
        let blank = blank_memory(Some("c1".into()), Some("cv1".into()));
        assert_eq!(blank.id, None);
        assert_eq!(blank.character_id, Some("c1".into()));
        assert_eq!(blank.conversation_id, Some("cv1".into()));
        assert_eq!(blank.kind, MemoryKind::Fact);
        assert_eq!(blank.content, "");
        assert_eq!(blank.importance, 5);
        assert!(!blank.pinned);
    }

    #[test]
    fn test_dialog_state_crud_and_editing() {
        let entries = sample_entries();
        let mut state = MemoryDialogState::new();

        // 1. Filter changes
        let act = state.update(&entries, MemoryMessage::FilterCharacter(Some("char-1".into())));
        match act {
            MemoryAction::Refresh {
                character_id,
                conversation_id,
            } => {
                assert_eq!(character_id, Some("char-1".into()));
                assert_eq!(conversation_id, None);
            }
            _ => panic!("Expected MemoryAction::Refresh"),
        }
        assert_eq!(state.filter_character_id, Some("char-1".into()));

        // 2. Add memory
        let act = state.update(&entries, MemoryMessage::AddMemory);
        assert!(matches!(act, MemoryAction::None));
        assert!(state.editing_memory.is_some());
        let draft = state.editing_memory.as_ref().unwrap();
        assert_eq!(draft.character_id, Some("char-1".into()));
        assert_eq!(draft.importance, 5);

        // 3. Edit fields
        state.update(&entries, MemoryMessage::ContentChanged("Valuable ancient coin".into()));
        state.update(&entries, MemoryMessage::KindChanged(MemoryKind::Fact));
        state.update(&entries, MemoryMessage::ImportanceChanged(9));
        state.update(&entries, MemoryMessage::PinnedToggled(true));

        let draft = state.editing_memory.as_ref().unwrap();
        assert_eq!(draft.content, "Valuable ancient coin");
        assert_eq!(draft.kind, MemoryKind::Fact);
        assert_eq!(draft.importance, 9);
        assert!(draft.pinned);

        // 4. Save memory
        let save_act = state.update(&entries, MemoryMessage::SaveMemory);
        match save_act {
            MemoryAction::Save(input) => {
                assert_eq!(input.content, "Valuable ancient coin");
                assert_eq!(input.importance, 9);
                assert!(input.pinned);
            }
            _ => panic!("Expected MemoryAction::Save"),
        }
        assert!(state.editing_memory.is_none());

        // 5. Edit existing memory
        state.update(&entries, MemoryMessage::EditMemory("m2".into()));
        assert!(state.editing_memory.is_some());
        let editing = state.editing_memory.as_ref().unwrap();
        assert_eq!(editing.id, Some("m2".into()));
        assert_eq!(editing.content, "The party defeated the shadow beast at the river crossing.");

        // Cancel editing
        state.update(&entries, MemoryMessage::CancelEdit);
        assert!(state.editing_memory.is_none());

        // 6. Delete confirmation workflow
        state.update(&entries, MemoryMessage::RequestDelete("m3".into()));
        assert_eq!(state.delete_confirm_id, Some("m3".into()));

        // Cancel delete
        state.update(&entries, MemoryMessage::CancelDelete);
        assert_eq!(state.delete_confirm_id, None);

        // Request and confirm delete
        state.update(&entries, MemoryMessage::RequestDelete("m3".into()));
        let del_act = state.update(&entries, MemoryMessage::ConfirmDelete("m3".into()));
        match del_act {
            MemoryAction::Delete(id) => assert_eq!(id, "m3"),
            _ => panic!("Expected MemoryAction::Delete"),
        }
        assert_eq!(state.delete_confirm_id, None);
    }

    #[test]
    fn test_memory_dialog_view_creation() {
        let entries = sample_entries();
        let characters = vec![Character {
            id: "char-1".into(),
            name: "Lyra".into(),
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
        let conversations = vec![Conversation {
            id: "conv-1".into(),
            title: "Silverwood quest".into(),
            kind: chatty_protocol::ConversationKind::Direct,
            participant_ids: vec!["char-1".into()],
            state: String::new(),
            summary: String::new(),
            revision: 1,
        }];
        let state = MemoryDialogState::new();

        let view_state = MemoryViewState::new(
            &entries,
            &characters,
            &conversations,
            &state,
            true,
            false,
            false,
        );

        let _element: Element<'_, MemoryMessage> = memory_dialog_view(&view_state);
    }
}
