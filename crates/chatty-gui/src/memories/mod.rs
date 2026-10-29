//! Character memories: dialog state, filtering, and messages.

#[cfg(test)]
mod tests;
mod view;

pub use view::*;

use chatty_protocol::{MemoryEntry, MemoryInput, MemoryKind};
use iced::widget::text_editor;

/// Broker importance scale; retrieval ranks by it, and learned memories use 50.
pub const MAX_IMPORTANCE: u8 = 100;
const DEFAULT_IMPORTANCE: u8 = 50;
/// Tallest the content editor grows before it scrolls.
pub const EDITOR_MAX_HEIGHT: f32 = 180.0;

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
    /// Cursor, selection, or edit in the content editor.
    Edit(text_editor::Action),
    /// Memory kind changed in editor.
    KindChanged(MemoryKind),
    /// Linked character changed in editor (None = Global).
    CharacterChanged(Option<String>),
    /// Linked conversation changed in editor (None = Global).
    ConversationChanged(Option<String>),
    /// Pinned state toggled in editor.
    PinnedToggled(bool),
    /// Importance rating changed in editor (0..=100).
    ImportanceChanged(u8),
    /// Pin or unpin a saved memory without opening the editor.
    TogglePin(String),
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
#[derive(Debug, Default)]
pub struct MemoryDialogState {
    pub filter_character_id: Option<String>,
    pub filter_conversation_id: Option<String>,
    pub filter_kind: Option<MemoryKind>,
    pub search_query: String,
    pub editing_memory: Option<MemoryInput>,
    /// Buffer for `editing_memory.content`; reloaded whenever the draft is replaced.
    pub editor: text_editor::Content,
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
                self.start_editing(blank_memory(
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
                    self.start_editing(memory_input(entry));
                }
                MemoryAction::None
            }
            MemoryMessage::Edit(action) => {
                // The app calls `perform` directly so the scroll task reaches the runtime.
                let _ = self.perform(action);
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
                    input.importance = importance.min(MAX_IMPORTANCE);
                }
                MemoryAction::None
            }
            MemoryMessage::TogglePin(id) => match memories.iter().find(|m| m.id == id) {
                Some(entry) => MemoryAction::Save(MemoryInput {
                    pinned: !entry.pinned,
                    ..memory_input(entry)
                }),
                None => MemoryAction::None,
            },
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

    fn start_editing(&mut self, input: MemoryInput) {
        self.editor = text_editor::Content::with_text(&input.content);
        self.delete_confirm_id = None;
        self.editing_memory = Some(input);
    }

    /// Apply an editor action and copy edits back into the draft.
    ///
    /// Typing and keyboard movement return a task that scrolls the editor to keep
    /// the cursor in view; clicks and drags leave it alone.
    pub fn perform(&mut self, action: text_editor::Action) -> iced::Task<MemoryMessage> {
        let follow = matches!(
            action,
            text_editor::Action::Edit(_) | text_editor::Action::Move(_)
        );
        let edit = action.is_edit();
        self.editor.perform(action);
        if edit && let Some(input) = &mut self.editing_memory {
            input.content = self.editor.text();
        }
        if !follow {
            return iced::Task::none();
        }
        // The text offset of the cursor approximates its height in the wrapped
        // layout, which the editor does not expose.
        let cursor = self.editor.cursor().position;
        let (mut before, mut total) = (0, 0);
        for index in 0..self.editor.line_count() {
            let len = self.editor.line(index).map_or(0, |line| line.text.len()) + 1;
            if index < cursor.line {
                before += len;
            }
            total += len;
        }
        let y = (before + cursor.column) as f32 / total.saturating_sub(1).max(1) as f32;
        iced::widget::operation::snap_to(
            editor_scroll_id(),
            iced::widget::scrollable::RelativeOffset {
                x: None,
                y: Some(y.min(1.0)),
            },
        )
    }
}

pub(crate) fn editor_scroll_id() -> iced::widget::Id {
    iced::widget::Id::new("memory-content")
}

fn memory_input(entry: &MemoryEntry) -> MemoryInput {
    MemoryInput {
        id: Some(entry.id.clone()),
        conversation_id: entry.conversation_id.clone(),
        character_id: entry.character_id.clone(),
        kind: entry.kind,
        content: entry.content.clone(),
        importance: entry.importance,
        pinned: entry.pinned,
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
        importance: DEFAULT_IMPORTANCE,
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
