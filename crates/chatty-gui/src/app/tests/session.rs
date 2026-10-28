use super::Harness;
use crate::app::Message;
use crate::characters::image_handle;
use crate::conversation::ConversationMessage;
use chatty_protocol::{DeltaOperation, GenerationContext, MessageType, Response, Role, StateDelta};

#[test]
fn context_usage_tracks_its_conversation_and_clears_on_sign_out() {
    let mut app = Harness::demo();
    let mut other = app.conversations[0].clone();
    other.id = "other".into();
    app.conversations.push(other);
    let context = GenerationContext {
        conversation_id: "other".into(),
        prompt_tokens: 1000,
        completion_tokens: 0,
        context_tokens: 8192,
        estimated: true,
    };
    app.respond(
        7,
        &Response::GenerationStarted {
            message_id: "reply".into(),
            character_id: "assistant".into(),
            context: context.clone(),
        },
    );
    assert_eq!(app.selected_conversation.as_deref(), Some("demo"));
    assert_eq!(app.context_usage["demo"].prompt_tokens, 2800);
    assert_eq!(app.context_usage["other"], context);

    let final_context = GenerationContext {
        completion_tokens: 200,
        estimated: false,
        ..context
    };
    app.frame(
        MessageType::StreamEnd,
        7,
        &Response::GenerationFinished {
            message_id: "reply".into(),
            revision: 10,
            cancelled: false,
            context: final_context.clone(),
        },
    );
    assert_eq!(app.context_usage["other"], final_context);
    assert_eq!(app.context_usage["demo"].prompt_tokens, 2800);

    app.apply_delta(StateDelta {
        revision: 11,
        entity_type: "conversation".into(),
        entity_id: "other".into(),
        operation: DeltaOperation::Delete,
        changed_fields: vec![],
    });
    assert!(!app.context_usage.contains_key("other"));
    app.update(Message::SignOut);
    assert!(app.context_usage.is_empty());
}

#[test]
fn long_network_errors_move_to_the_status_tooltip() {
    let mut app = Harness::new();
    let detail = "2026-09-14 15:43:39 UTC · Offline: connection timed out";
    app.set_network_status(detail.into());
    assert_eq!(app.status, "Offline");
    assert_eq!(app.status_tooltip.as_deref(), Some(detail));
    app.set_network_status("Online · TLS 1.3".into());
    assert_eq!(app.status, "Online · TLS 1.3");
    assert!(app.status_tooltip.is_none());
}

#[test]
fn image_handles_clear_on_sign_out_and_account_change() {
    let mut app = Harness::demo();
    app.connected = true;
    let bytes = [1, 2, 3];
    let original = image_handle(&bytes).unwrap();
    app.update(ConversationMessage::InputChanged("hello".into()));
    assert_eq!(image_handle(&bytes).unwrap().id(), original.id());

    app.update(Message::SignOut);
    let after_sign_out = image_handle(&bytes).unwrap();
    assert_ne!(after_sign_out.id(), original.id());

    app.authenticated("new-session".into(), "new-owner".into(), Role::User, 0);
    assert_ne!(image_handle(&bytes).unwrap().id(), after_sign_out.id());
}

#[test]
fn sign_out_and_expiry_clear_account_state() {
    for expire in [false, true] {
        let mut app = Harness::demo();
        app.worlds_open = true;
        app.memory_dialog_open = true;
        if expire {
            app.event(crate::network::Event::SessionExpired);
            assert!(app.error.is_some(), "expiry is reported");
        } else {
            app.update(Message::SignOut);
        }
        assert!(app.token.is_empty());
        assert!(app.role.is_none());
        assert!(app.memories.is_empty());
        assert!(!app.worlds_open && !app.memory_dialog_open);
        assert!(app.admin.admin_log.content.is_empty());
    }
}
