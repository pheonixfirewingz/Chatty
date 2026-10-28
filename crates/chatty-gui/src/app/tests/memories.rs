use super::Harness;
use crate::memories::MemoryMessage;
use chatty_protocol::{MemoryKind, Request, Response};

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
