use super::{Harness, png};
use crate::network::Command;
use chatty_protocol::{
    CharacterImage, ChatMessage, DeltaOperation, DeltaPayload, GenerationContext, GenerationStatus,
    MessageType, Request, Response, StateDelta, StreamChunk, encode,
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
