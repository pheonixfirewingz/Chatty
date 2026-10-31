use super::{Harness, png};
use crate::network::Command;
use chatty_protocol::{
    CharacterImage, ChatMessage, DeltaOperation, DeltaPayload, ErrorCode, GenerationContext,
    GenerationStatus, MessageType, Request, Response, StateDelta, StreamChunk, WireError, encode,
};

fn add_happy_image(app: &mut Harness) {
    app.characters[0].images.push(CharacterImage {
        id: "demo-happy".into(),
        label: "happy".into(),
        data: png(128),
    });
}

#[test]
fn continue_creates_a_message_and_retry_creates_a_variant() {
    let mut app = Harness::demo();
    app.submit_message();
    assert!(matches!(
        app.requests().as_slice(),
        [Request::Generate {
            parent_id: None,
            ..
        }]
    ));

    let response = app.messages.last().unwrap().clone();
    app.regenerate(&response);
    assert!(matches!(
        app.requests().as_slice(),
        [Request::Generate { parent_id: Some(parent), .. }] if parent == &response.id
    ));
}

#[test]
fn typed_messages_are_sent_before_generation() {
    let mut app = Harness::demo();
    app.input = "hello".into();
    app.submit_message();
    assert!(app.input.is_empty());
    assert!(matches!(
        app.commands().as_slice(),
        [Command::SendThenGenerate { message, generate }]
            if matches!(**message, Request::SendMessage { ref content, .. } if content == "hello")
                && matches!(**generate, Request::Generate { .. })
    ));
}

#[test]
fn latest_persisted_reply_changes_the_visible_emotion() {
    let mut app = Harness::demo();
    add_happy_image(&mut app);
    app.messages.push(ChatMessage {
        id: "m3".into(),
        author_type: "character".into(),
        author_id: Some("assistant".into()),
        content: "A newly persisted reply".into(),
        parent_id: None,
        selected_variant_id: None,
        character_image_id: Some("demo-happy".into()),
        created_at: String::new(),
        revision: 3,
        variants: vec![],
    });

    let portrait = app.emotion_portrait().unwrap();
    assert_eq!((portrait.1, portrait.2), ("happy", "demo-happy"));
}

#[test]
fn generation_delta_updates_the_emotion_panel_immediately() {
    let mut app = Harness::demo();
    add_happy_image(&mut app);
    let changed_fields = encode(&DeltaPayload::Message {
        conversation_id: "demo".into(),
        author_type: "character".into(),
        author_id: Some("assistant".into()),
        content: "The generated reply remains visible".into(),
        parent_id: None,
        selected_variant_id: None,
        character_image_id: Some("demo-happy".into()),
    })
    .unwrap();
    app.frame(
        MessageType::Delta,
        42,
        &StateDelta {
            revision: 4,
            entity_type: "message".into(),
            entity_id: "generated-reply".into(),
            operation: DeltaOperation::Add,
            changed_fields,
        },
    );

    assert_eq!(
        app.messages.last().map(|m| m.content.as_str()),
        Some("The generated reply remains visible")
    );
    let portrait = app.emotion_portrait().unwrap();
    assert_eq!((portrait.1, portrait.2), ("happy", "demo-happy"));
}

#[test]
fn thinking_status_shows_until_reply_text_streams() {
    let mut app = Harness::demo();
    app.respond(
        7,
        &Response::GenerationStarted {
            message_id: "reply".into(),
            character_id: "assistant".into(),
            context: GenerationContext {
                conversation_id: "demo".into(),
                prompt_tokens: 10,
                completion_tokens: 0,
                context_tokens: 4096,
                estimated: true,
            },
        },
    );
    app.respond(
        7,
        &Response::GenerationStatus {
            message_id: "reply".into(),
            status: GenerationStatus::Thinking,
        },
    );
    assert_eq!(app.generation_status, Some(GenerationStatus::Thinking));

    app.frame(
        MessageType::StreamChunk,
        7,
        &StreamChunk {
            message_id: "reply".into(),
            sequence: 1,
            text: "Hello".into(),
        },
    );
    assert_eq!(app.generation_status, None);
    assert_eq!(app.stream_text, "Hello");
}

/// Push an image message and start its fetch as request 41.
fn start_image_fetch(app: &mut Harness) {
    app.messages.push(ChatMessage {
        id: "img".into(),
        author_type: "image".into(),
        author_id: None,
        content: "a lake".into(),
        parent_id: None,
        selected_variant_id: None,
        character_image_id: None,
        created_at: String::new(),
        revision: 9,
        variants: Vec::new(),
    });
    app.request_message_images();
    assert!(matches!(
        app.requests().as_slice(),
        [Request::GetMessageImage { message_id, .. }] if message_id == "img"
    ));
    app.event(crate::network::Event::MessageImageRequestStarted {
        request_id: 41,
        message_id: "img".into(),
    });
}

#[test]
fn missing_message_image_is_marked_unavailable_quietly() {
    let mut app = Harness::demo();
    start_image_fetch(&mut app);
    app.active_request = Some(7);
    app.fail_with(41, ErrorCode::NotFound, "message image not found");
    assert!(app.error.is_none());
    assert_eq!(app.active_request, Some(7));
    assert_eq!(app.message_images.get("img").map(Vec::len), Some(0));
    app.request_message_images();
    assert!(app.requests().is_empty());
}

fn busy() -> WireError {
    WireError {
        code: ErrorCode::BackendUnavailable,
        message: "backend busy".into(),
        retryable: true,
    }
}

#[test]
fn temporary_message_image_error_is_retried_after_a_delay() {
    let mut app = Harness::demo();
    start_image_fetch(&mut app);
    app.active_request = Some(7);
    // The failure schedules one delayed retry task.
    assert_eq!(app.frame_units(MessageType::Error, 41, &busy()), 1);
    assert!(app.error.is_none());
    assert_eq!(app.active_request, Some(7));
    assert!(!app.message_images.contains_key("img"));
    // Deltas do not refetch it before the retry is due.
    app.request_message_images();
    assert!(app.requests().is_empty());
    app.update(crate::app::Message::RetryMessageImages);
    assert!(matches!(
        app.requests().as_slice(),
        [Request::GetMessageImage { message_id, .. }] if message_id == "img"
    ));
}

#[test]
fn repeated_temporary_message_image_errors_give_up() {
    let mut app = Harness::demo();
    start_image_fetch(&mut app);
    for attempt in 1..=4u64 {
        let request_id = 40 + attempt;
        if attempt > 1 {
            app.update(crate::app::Message::RetryMessageImages);
            assert_eq!(app.requests().len(), 1);
            app.event(crate::network::Event::MessageImageRequestStarted {
                request_id,
                message_id: "img".into(),
            });
        }
        let tasks = app.frame_units(MessageType::Error, request_id, &busy());
        assert_eq!(tasks, usize::from(attempt < 4));
    }
    // The last attempt marks the image unavailable; nothing fetches it again.
    assert_eq!(app.message_images.get("img").map(Vec::len), Some(0));
    app.update(crate::app::Message::RetryMessageImages);
    assert!(app.requests().is_empty());
}

#[test]
fn unauthorized_message_image_errors_are_shown_once() {
    let mut app = Harness::demo();
    start_image_fetch(&mut app);
    app.fail_with(41, ErrorCode::Unauthorized, "unauthorized");
    assert_eq!(
        app.error.as_ref().map(|notice| notice.message.as_str()),
        Some("unauthorized")
    );
    assert!(!app.message_images.contains_key("img"));
    // After the session expired, later failures keep the expiry notice.
    app.event(crate::network::Event::SessionExpired);
    let expired = app.error.clone().map(|notice| notice.message);
    app.messages.clear();
    start_image_fetch(&mut app);
    app.fail_with(41, ErrorCode::Unauthorized, "unauthorized");
    assert_eq!(app.error.clone().map(|notice| notice.message), expired);
}

#[test]
fn switching_conversations_by_any_path_opens_at_the_bottom() {
    let mut app = Harness::demo();
    let conversation = app.conversations[0].clone();
    let view = |messages: Vec<ChatMessage>| {
        Response::ConversationView(chatty_protocol::ConversationView {
            conversation: conversation.clone(),
            messages,
        })
    };
    let messages = app.messages.clone();
    // A chat that was not shown opens at its newest message.
    assert_eq!(
        app.frame_units(MessageType::Response, 5, &view(messages.clone())),
        1
    );
    app.update(scrolled(200.0, 950.0));
    assert!(!app.chat_at_bottom);
    // Refreshing the same chat keeps the reading position.
    assert_eq!(
        app.frame_units(MessageType::Response, 6, &view(messages.clone())),
        0
    );
    assert!(!app.chat_at_bottom);
    // Closing it, as deletion does, makes the next view a switch.
    app.close_conversation();
    assert_eq!(
        app.frame_units(MessageType::Response, 7, &view(messages)),
        1
    );
    assert!(app.chat_at_bottom);
}

#[test]
fn permanent_message_image_error_is_not_refetched() {
    let mut app = Harness::demo();
    start_image_fetch(&mut app);
    app.fail_with(41, ErrorCode::Internal, "decryption failed");
    assert!(app.error.is_none());
    assert_eq!(app.message_images.get("img").map(Vec::len), Some(0));
    app.request_message_images();
    assert!(app.requests().is_empty());
}

#[test]
fn forbidden_message_image_error_is_shown_once() {
    let mut app = Harness::demo();
    start_image_fetch(&mut app);
    app.fail_with(41, ErrorCode::Forbidden, "forbidden");
    assert_eq!(
        app.error.as_ref().map(|notice| notice.message.as_str()),
        Some("forbidden")
    );
    assert_eq!(app.message_images.get("img").map(Vec::len), Some(0));
    app.request_message_images();
    assert!(app.requests().is_empty());
}

fn scrolled(from_top: f32, from_bottom: f32) -> crate::app::Message {
    crate::app::Message::Conversation(crate::conversation::ConversationMessage::ChatScrolled {
        from_top,
        from_bottom,
    })
}

#[test]
fn chat_follows_new_content_only_while_at_the_bottom() {
    let mut app = Harness::demo();
    assert!(app.chat_at_bottom);
    // Content growth while following: no task, still following.
    assert_eq!(app.update_units(scrolled(900.0, 0.0)), 0);
    assert!(app.chat_at_bottom);
    // The user scrolls up: stop following and keep the position under the
    // top anchor.
    assert_eq!(app.update_units(scrolled(500.0, 400.0)), 1);
    assert!(!app.chat_at_bottom);
    // Tokens stream and images load below: the position stays put.
    assert_eq!(app.update_units(scrolled(500.0, 650.0)), 0);
    assert!(!app.chat_at_bottom);
    // Back at the bottom, within the tolerance: follow again.
    assert_eq!(app.update_units(scrolled(1147.0, 3.0)), 1);
    assert!(app.chat_at_bottom);
    assert_eq!(app.update_units(scrolled(1150.0, 0.0)), 0);
    // Sending jumps to the bottom even when scrolled up.
    app.update(scrolled(200.0, 950.0));
    assert!(!app.chat_at_bottom);
    app.input = "Hello".into();
    assert_eq!(
        app.update_units(crate::app::Message::Conversation(
            crate::conversation::ConversationMessage::SubmitMessage
        )),
        1
    );
    assert!(app.chat_at_bottom);
}

#[test]
fn message_edits_use_a_multi_line_editor() {
    use crate::conversation::ConversationMessage;
    use iced::widget::text_editor::{Action, Edit};
    let mut app = Harness::demo();
    let id = app.messages[0].id.clone();
    // Starting an edit returns the task that focuses the new editor.
    assert_eq!(
        app.update_units(crate::app::Message::Conversation(
            ConversationMessage::StartEditMessage {
                id: id.clone(),
                content: "Hi".into(),
            },
        )),
        1
    );
    for action in [
        Action::Move(iced::widget::text_editor::Motion::DocumentEnd),
        Action::Edit(Edit::Enter),
        Action::Edit(Edit::Insert('x')),
    ] {
        app.update(crate::app::Message::Conversation(
            ConversationMessage::EditAction(action),
        ));
    }
    app.update(crate::app::Message::Conversation(
        ConversationMessage::SaveMessageEdit,
    ));
    assert!(app.editing_message_id.is_none());
    assert!(matches!(
        app.requests().as_slice(),
        [Request::UpdateMessage { message_id, content, .. }]
            if message_id == &id && content == "Hi\nx"
    ));
}
