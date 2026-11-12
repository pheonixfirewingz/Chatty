//! Character memories: dialog state, filtering, and messages.

#[cfg(test)]
mod tests;
mod view;

pub use view::*;

use chatty_protocol::{MemoryEntry, MemoryInput, MemoryKind, MemoryStatus};
use iced::widget::text_editor;
use std::collections::{BTreeMap, BTreeSet};

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
    /// Show or hide the earlier versions of a memory.
    ToggleHistory(String),
    /// Show or hide the chat messages a memory was learned from.
    ToggleSources(String),
    /// Show or hide retired memories and orphaned earlier versions.
    ToggleRetired,
    /// Make a superseded or retired memory active again.
    Restore(String),
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
    /// Memories whose earlier versions are shown.
    pub expanded_history: BTreeSet<String>,
    /// Memories whose source messages are shown.
    pub expanded_sources: BTreeSet<String>,
    pub show_retired: bool,
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
            MemoryMessage::ToggleHistory(id) => {
                toggle(&mut self.expanded_history, id);
                MemoryAction::None
            }
            MemoryMessage::ToggleSources(id) => {
                toggle(&mut self.expanded_sources, id);
                MemoryAction::None
            }
            MemoryMessage::ToggleRetired => {
                self.show_retired = !self.show_retired;
                MemoryAction::None
            }
            MemoryMessage::Restore(id) => match memories.iter().find(|m| m.id == id) {
                // Saving an inactive memory makes it active again on the broker.
                Some(entry) if entry.status != MemoryStatus::Active => {
                    MemoryAction::Save(memory_input(entry))
                }
                _ => MemoryAction::None,
            },
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
        let (edit, task) =
            crate::ui::perform_editor_action(&mut self.editor, action, editor_scroll_id());
        if edit && let Some(input) = &mut self.editing_memory {
            input.content = self.editor.text();
        }
        task
    }
}

fn toggle(set: &mut BTreeSet<String>, id: String) {
    if !set.remove(&id) {
        set.insert(id);
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

fn matches_filters(
    memory: &MemoryEntry,
    character_id: Option<&str>,
    conversation_id: Option<&str>,
    kind: Option<MemoryKind>,
    query: &str,
) -> bool {
    character_id.is_none_or(|id| memory.character_id.as_deref() == Some(id))
        && conversation_id.is_none_or(|id| {
            memory
                .conversation_id
                .as_deref()
                .is_none_or(|own| own == id)
        })
        && kind.is_none_or(|kind| memory.kind == kind)
        && (query.is_empty() || memory.content.to_lowercase().contains(query))
}

/// Filters active memories based on character, conversation, kind, and search text.
pub fn filter_memories<'a>(
    memories: &'a [MemoryEntry],
    character_id: Option<&str>,
    conversation_id: Option<&str>,
    kind: Option<MemoryKind>,
    query: &str,
) -> Vec<&'a MemoryEntry> {
    let query = query.trim().to_lowercase();
    memories
        .iter()
        .filter(|m| m.status == MemoryStatus::Active)
        .filter(|m| matches_filters(m, character_id, conversation_id, kind, &query))
        .collect()
}

/// Inactive memories that no listed memory replaced: retired memories, and
/// earlier versions whose replacement is gone. Uses the same filters.
pub fn filter_retired<'a>(
    memories: &'a [MemoryEntry],
    character_id: Option<&str>,
    conversation_id: Option<&str>,
    kind: Option<MemoryKind>,
    query: &str,
) -> Vec<&'a MemoryEntry> {
    let query = query.trim().to_lowercase();
    let ids = memories
        .iter()
        .map(|m| m.id.as_str())
        .collect::<BTreeSet<_>>();
    memories
        .iter()
        .filter(|m| match m.status {
            MemoryStatus::Active => false,
            MemoryStatus::Retired => true,
            MemoryStatus::Superseded => m
                .superseded_by
                .as_deref()
                .is_none_or(|successor| !ids.contains(successor)),
        })
        .filter(|m| matches_filters(m, character_id, conversation_id, kind, &query))
        .collect()
}

/// Earlier versions of each memory, keyed by the ID that replaced them.
/// Built once per view so each card finds its history without a scan.
pub fn predecessors(memories: &[MemoryEntry]) -> BTreeMap<&str, Vec<&MemoryEntry>> {
    let mut map = BTreeMap::<&str, Vec<&MemoryEntry>>::new();
    for memory in memories {
        if memory.status == MemoryStatus::Superseded
            && let Some(successor) = memory.superseded_by.as_deref()
        {
            map.entry(successor).or_default().push(memory);
        }
    }
    map
}

/// All earlier versions of `id`, newest first, following replacement links
/// back through every generation. Bounded by the listed history.
pub fn memory_history<'a>(
    predecessors: &BTreeMap<&str, Vec<&'a MemoryEntry>>,
    id: &str,
) -> Vec<&'a MemoryEntry> {
    let mut history = Vec::<&MemoryEntry>::new();
    let mut pending = vec![id.to_owned()];
    while let Some(current) = pending.pop() {
        for memory in predecessors.get(current.as_str()).into_iter().flatten() {
            if memory.id != id && !history.iter().any(|seen| seen.id == memory.id) {
                history.push(memory);
                pending.push(memory.id.clone());
            }
        }
    }
    history.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    history
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
