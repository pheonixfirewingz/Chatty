use super::Harness;
use crate::memories::MemoryMessage;
use chatty_protocol::{Conversation, ConversationKind, MemoryKind, Request, Response};

#[test]
fn memory_condense_flow_updates_status_and_recovers_on_error() {
    let mut app = Harness::new();
    app.token = "session-token".into();
    app.memory_state.filter_character_id = Some("char-1".into());
    app.memory_state.filter_conversation_id = Some("conv-1".into());
    app.memory_state.filter_kind = Some(MemoryKind::Fact);

    app.update(MemoryMessage::Condense);
    assert!(app.memory_state.condensing);
    assert_eq!(
        app.memory_state.status_message.as_deref(),
        Some("Condensing memories…")
    );
    match app.requests().as_slice() {
        [
            Request::CondenseMemories {
                session_token,
                conversation_id,
                character_id,
                kind,
            },
        ] => {
            assert_eq!(session_token, "session-token");
            assert_eq!(conversation_id.as_deref(), Some("conv-1"));
            assert_eq!(character_id.as_deref(), Some("char-1"));
            assert_eq!(*kind, Some(MemoryKind::Fact));
        }
        other => panic!("unexpected requests: {other:?}"),
    }

    app.respond(
        1,
        &Response::MemoriesCondensed {
            before: 4,
            after: 2,
        },
    );
    assert!(!app.memory_state.condensing);
    assert_eq!(
        app.memory_state.status_message.as_deref(),
        Some("Condensed 4 memories into 2.")
    );
    assert!(matches!(
        app.requests().as_slice(),
        [Request::GetAccountUsage { .. }]
    ));

    app.update(MemoryMessage::Condense);
    assert!(app.memory_state.condensing);
    app.commands();
    app.fail(2, "model busy");
    assert!(!app.memory_state.condensing);
    assert_eq!(app.memory_state.status_message, None);
}

#[test]
fn memory_extraction_reports_operation_counts() {
    let mut app = Harness::new();
    app.token = "session-token".into();
    app.selected_conversation = Some("conv-1".into());
    app.conversations.push(Conversation {
        id: "conv-1".into(),
        title: "Group".into(),
        kind: ConversationKind::GroupAutomatic,
        participant_ids: vec!["char-1".into(), "char-2".into()],
        state: String::new(),
        summary: String::new(),
        revision: 1,
    });
    // A filter outside the chat extracts for every participant.
    app.memory_state.filter_character_id = Some("elsewhere".into());
    app.update(MemoryMessage::ExtractFromChat);
    assert!(app.requests().iter().any(|request| matches!(
        request,
        Request::ExtractMemory {
            character_id: None,
            ..
        }
    )));
    app.memory_state.filter_character_id = Some("char-1".into());
    app.update(MemoryMessage::ExtractFromChat);
    assert_eq!(
        app.memory_state.status_message.as_deref(),
        Some("Learning from chat…")
    );
    assert!(app.requests().iter().any(|request| matches!(
        request,
        Request::ExtractMemory { conversation_id, character_id, .. }
            if conversation_id == "conv-1" && character_id.as_deref() == Some("char-1")
    )));

    app.respond(
        2,
        &Response::MemoriesExtracted {
            added: 2,
            updated: 1,
            retired: 0,
        },
    );
    assert_eq!(
        app.memory_state.status_message.as_deref(),
        Some("Memories: 2 learned, 1 updated.")
    );
    assert!(matches!(
        app.requests().as_slice(),
        [Request::GetAccountUsage { .. }]
    ));

    app.respond(
        3,
        &Response::MemoriesExtracted {
            added: 0,
            updated: 0,
            retired: 0,
        },
    );
    assert_eq!(
        app.memory_state.status_message.as_deref(),
        Some("Nothing new to remember from this chat.")
    );
}
