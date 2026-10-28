//! Character memories: dialog state, filtering, and messages.

#[cfg(test)]
mod tests;
mod view;

pub use view::*;

use chatty_protocol::{MemoryEntry, MemoryInput, MemoryKind};

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
    /// Merge redundant unpinned memories in the current scope in one click.
    Condense,
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

/// Outgoing actions resulting from handling a `MemoryMessage`.
#[derive(Debug, Clone)]
pub enum MemoryAction {
    None,
    Save(MemoryInput),
    Delete(String),
    ExtractFromChat,
    Condense,
    Refresh,
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
    pub condensing: bool,
    pub status_message: Option<String>,
}

impl MemoryDialogState {
    pub fn with_filters(character_id: Option<String>, conversation_id: Option<String>) -> Self {
        Self {
            filter_character_id: character_id,
            filter_conversation_id: conversation_id,
            ..Default::default()
        }
    }

    /// Handles a `MemoryMessage` and returns an outgoing `MemoryAction`.
    pub fn update(&mut self, memories: &[MemoryEntry], message: MemoryMessage) -> MemoryAction {
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
                self.filter_character_id = character_id;
                MemoryAction::Refresh
            }
            MemoryMessage::FilterConversation(conversation_id) => {
                self.filter_conversation_id = conversation_id;
                MemoryAction::Refresh
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
            MemoryMessage::Condense => {
                if self.condensing {
                    MemoryAction::None
                } else {
                    self.condensing = true;
                    MemoryAction::Condense
                }
            }
            MemoryMessage::Refresh => MemoryAction::Refresh,
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

/// Constructs a blank `MemoryInput` pre-populated with optional scope and default values.
pub fn blank_memory(character_id: Option<String>, conversation_id: Option<String>) -> MemoryInput {
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
            if let Some(cid) = character_id
                && m.character_id.as_deref() != Some(cid)
            {
                return false;
            }
            if let Some(cvid) = conversation_id
                && m.conversation_id.as_deref().is_some_and(|id| id != cvid)
            {
                return false;
            }
            if let Some(k) = kind
                && m.kind != k
            {
                return false;
            }
            q.is_empty() || m.content.to_lowercase().contains(&q)
        })
        .collect()
}

/// Returns whether the selected scope contains at least two unpinned memories
/// sharing a `(conversation_id, character_id, kind)` group that can be condensed.
pub fn can_condense_memories(
    memories: &[MemoryEntry],
    character_id: Option<&str>,
    conversation_id: Option<&str>,
    kind: Option<MemoryKind>,
) -> bool {
    let scoped = filter_memories(memories, character_id, conversation_id, kind, "");
    let unpinned: Vec<&MemoryEntry> = scoped.into_iter().filter(|m| !m.pinned).collect();
    for (idx, left) in unpinned.iter().enumerate() {
        if unpinned[idx + 1..].iter().any(|right| {
            left.conversation_id == right.conversation_id
                && left.character_id == right.character_id
                && left.kind == right.kind
        }) {
            return true;
        }
    }
    false
}
