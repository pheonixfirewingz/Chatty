use super::*;
use chatty_protocol::MemorySource;
use chatty_protocol::{Character, Conversation};
use iced::Element;

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
    assert_eq!(
        filtered
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["m1", "m3"]
    );
    let other_chat = filter_memories(&entries, Some("char-1"), Some("new-chat"), None, "");
    assert_eq!(other_chat.len(), 1);
    assert_eq!(other_chat[0].id, "m3");
}

#[test]
fn one_click_condense_dispatches_immediately_and_blocks_duplicate_clicks() {
    let mut entries = sample_entries();
    // Distinct groups or pinned entries alone cannot be condensed.
    assert!(!can_condense_memories(&entries, None, None, None));

    // Adding a second unpinned memory in the same (conversation, character, kind) group enables it.
    entries.push(MemoryEntry {
        id: "m4".into(),
        conversation_id: None,
        character_id: Some("char-1".into()),
        kind: MemoryKind::Relationship,
        content: "Lyra admires courageous travelers.".into(),
        importance: 6,
        pinned: false,
        confidence: 0.8,
        source: MemorySource::Automatic,
        source_message_ids: vec![],
        created_at: "2026-09-03T15:00:00Z".into(),
        updated_at: "2026-09-03T15:00:00Z".into(),
        revision: 2,
    });
    assert!(can_condense_memories(&entries, None, None, None));
    assert!(can_condense_memories(
        &entries,
        Some("char-1"),
        None,
        Some(MemoryKind::Relationship),
    ));
    assert!(!can_condense_memories(
        &entries,
        Some("char-1"),
        None,
        Some(MemoryKind::Fact),
    ));

    let mut state = MemoryDialogState::default();
    assert!(matches!(
        state.update(&entries, MemoryMessage::Condense),
        MemoryAction::Condense
    ));
    assert!(state.condensing);
    assert!(matches!(
        state.update(&entries, MemoryMessage::Condense),
        MemoryAction::None
    ));
}

#[test]
fn test_memory_message_variants() {
    let msg_search = MemoryMessage::SearchChanged("archer".into());
    assert_eq!(msg_search, MemoryMessage::SearchChanged("archer".into()));

    let msg_clear = MemoryMessage::ClearSearch;
    assert_eq!(msg_clear, MemoryMessage::ClearSearch);

    let msg_char = MemoryMessage::FilterCharacter(Some("char-1".into()));
    assert_eq!(
        msg_char,
        MemoryMessage::FilterCharacter(Some("char-1".into()))
    );

    let msg_conv = MemoryMessage::FilterConversation(Some("conv-1".into()));
    assert_eq!(
        msg_conv,
        MemoryMessage::FilterConversation(Some("conv-1".into()))
    );

    let msg_kind = MemoryMessage::FilterKind(Some(MemoryKind::Fact));
    assert_eq!(msg_kind, MemoryMessage::FilterKind(Some(MemoryKind::Fact)));

    let msg_add = MemoryMessage::AddMemory;
    assert_eq!(msg_add, MemoryMessage::AddMemory);

    let msg_extract = MemoryMessage::ExtractFromChat;
    assert_eq!(msg_extract, MemoryMessage::ExtractFromChat);

    let msg_condense = MemoryMessage::Condense;
    assert_eq!(msg_condense, MemoryMessage::Condense);

    let msg_refresh = MemoryMessage::Refresh;
    assert_eq!(msg_refresh, MemoryMessage::Refresh);

    let msg_edit = MemoryMessage::EditMemory("m1".into());
    assert_eq!(msg_edit, MemoryMessage::EditMemory("m1".into()));

    let msg_content = MemoryMessage::ContentChanged("new text".into());
    assert_eq!(
        msg_content,
        MemoryMessage::ContentChanged("new text".into())
    );

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
    let mut state = MemoryDialogState::default();

    // 1. Filter changes
    let act = state.update(
        &entries,
        MemoryMessage::FilterCharacter(Some("char-1".into())),
    );
    assert!(matches!(act, MemoryAction::Refresh));
    assert_eq!(state.filter_character_id, Some("char-1".into()));
    assert_eq!(state.filter_conversation_id, None);

    // 2. Add memory
    let act = state.update(&entries, MemoryMessage::AddMemory);
    assert!(matches!(act, MemoryAction::None));
    assert!(state.editing_memory.is_some());
    let draft = state.editing_memory.as_ref().unwrap();
    assert_eq!(draft.character_id, Some("char-1".into()));
    assert_eq!(draft.importance, 5);

    // 3. Edit fields
    state.update(
        &entries,
        MemoryMessage::ContentChanged("Valuable ancient coin".into()),
    );
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
    assert_eq!(
        editing.content,
        "The party defeated the shadow beast at the river crossing."
    );

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
    let state = MemoryDialogState::default();

    let view_state =
        MemoryViewState::new(&entries, &characters, &conversations, &state, true, false);

    let _element: Element<'_, MemoryMessage> = memory_dialog_view(&view_state);
}
