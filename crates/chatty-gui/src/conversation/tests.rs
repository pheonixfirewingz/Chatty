use super::*;
use chatty_protocol::CharacterImage;
use chatty_protocol::conversation::Variant;
use iced::Element;

fn dummy_character(id: &str, name: &str) -> Character {
    Character {
        id: id.into(),
        name: name.into(),
        description: "A test bot".into(),
        personality: "Friendly companion".into(),
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
        revision: 1,
    }
}

fn dummy_message(id: &str, author_type: &str, content: &str) -> ChatMessage {
    ChatMessage {
        id: id.into(),
        author_type: author_type.into(),
        author_id: if author_type == "user" {
            None
        } else {
            Some("bot1".into())
        },
        content: content.into(),
        parent_id: None,
        selected_variant_id: None,
        character_image_id: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        revision: 1,
        variants: Vec::new(),
    }
}

#[test]
fn test_displayed_message_content_default() {
    let msg = dummy_message("m1", "user", "Hello world");
    assert_eq!(displayed_message_content(&msg), "Hello world");
}

#[test]
fn test_displayed_message_content_variant() {
    let mut msg = dummy_message("m1", "assistant", "Default response");
    msg.variants = vec![
        Variant {
            id: "v1".into(),
            content: "Alternative 1".into(),
            created_at: "2026-01-01".into(),
            revision: 1,
        },
        Variant {
            id: "v2".into(),
            content: "Alternative 2".into(),
            created_at: "2026-01-01".into(),
            revision: 1,
        },
    ];
    msg.selected_variant_id = Some("v2".into());
    assert_eq!(displayed_message_content(&msg), "Alternative 2");
}

#[test]
fn test_character_name() {
    let chars = vec![dummy_character("bot1", "Seraphina")];
    assert_eq!(character_name(&chars, Some("bot1")), "Seraphina");
    assert_eq!(character_name(&chars, Some("nonexistent")), "Assistant");
    assert_eq!(character_name(&chars, None), "Assistant");
}

#[test]
fn test_current_emotion_portrait_fallback() {
    let mut character = dummy_character("bot1", "Seraphina");
    character.default_image_id = Some("img1".into());
    character.images.push(CharacterImage {
        id: "img1".into(),
        label: "neutral".into(),
        data: vec![1, 2, 3],
    });
    let chars = vec![character];
    let messages = vec![dummy_message("m1", "assistant", "Hi")];

    let portrait = current_emotion_portrait(&chars, &messages, Some("c1"), Some("bot1"));
    assert!(portrait.is_some());
    let (name, label, id, data) = portrait.unwrap();
    assert_eq!(name, "Seraphina");
    assert_eq!(label, "neutral");
    assert_eq!(id, "img1");
    assert_eq!(data, &[1, 2, 3]);
    assert_eq!(data.as_ptr(), chars[0].images[0].data.as_ptr());
}

#[test]
fn test_conversation_view_empty_state() {
    let markdown = ConversationMarkdown::default();
    let view_state = ConversationViewState {
        markdown: &markdown,
        characters: &[],
        messages: &[],
        selected_conversation: None,
        selected_speaker: None,
        input: "",
        active_request: None,
        stream_text: "",
        typing_character: None,
        generation_status: None,
        context_usage: None,
        editing_message_id: None,
        editing_message_content: "",
        hovered_message_id: None,
        sidebar_visible: true,
        available_size: iced::Size::new(800.0, 720.0),
        glass_mode: false,
        message_images: &HashMap::new(),
        scene_image: ImageGeneration::Unavailable,
    };

    let _el: Element<'_, ConversationMessage> =
        conversation_view(&view_state, std::convert::identity);
}

#[test]
fn test_new_chat_dialog_rendering() {
    let chars = vec![dummy_character("c1", "Alice")];
    let view_state = NewChatViewState {
        is_open: true,
        characters: &chars,
        selected_character_id: Some("c1"),
        title: "My Chat",
        glass_mode: false,
    };

    let dialog = new_chat_dialog_view(&view_state, std::convert::identity);
    assert!(dialog.is_some());
}

#[test]
fn test_conversation_view_collapsed_sidebar() {
    let markdown = ConversationMarkdown::default();
    let view_state = ConversationViewState {
        markdown: &markdown,
        characters: &[],
        messages: &[],
        selected_conversation: None,
        selected_speaker: None,
        input: "",
        active_request: None,
        stream_text: "",
        typing_character: None,
        generation_status: None,
        context_usage: None,
        editing_message_id: None,
        editing_message_content: "",
        hovered_message_id: None,
        sidebar_visible: false,
        available_size: iced::Size::new(800.0, 720.0),
        glass_mode: false,
        message_images: &HashMap::new(),
        scene_image: ImageGeneration::Unavailable,
    };

    let _el: Element<'_, ConversationMessage> =
        conversation_view(&view_state, std::convert::identity);
}

#[test]
fn test_conversation_view_with_messages_hover() {
    let chars = vec![dummy_character("bot1", "Mara")];
    let msgs = vec![
        dummy_message("m1", "user", "Tell me about this place."),
        dummy_message(
            "m2",
            "assistant",
            "It has watched the valley for centuries.",
        ),
    ];
    let mut markdown = ConversationMarkdown::default();
    markdown.update(&msgs, "");
    let view_state_unhovered = ConversationViewState {
        markdown: &markdown,
        characters: &chars,
        messages: &msgs,
        selected_conversation: Some("c1"),
        selected_speaker: Some("bot1"),
        input: "",
        active_request: None,
        stream_text: "",
        typing_character: None,
        generation_status: None,
        context_usage: None,
        editing_message_id: None,
        editing_message_content: "",
        hovered_message_id: None,
        sidebar_visible: false,
        available_size: iced::Size::new(800.0, 720.0),
        glass_mode: false,
        message_images: &HashMap::new(),
        scene_image: ImageGeneration::Unavailable,
    };
    let _el1: Element<'_, ConversationMessage> =
        conversation_view(&view_state_unhovered, std::convert::identity);

    let view_state_user_hover = ConversationViewState {
        hovered_message_id: Some("m1"),
        ..view_state_unhovered
    };
    let _el2: Element<'_, ConversationMessage> =
        conversation_view(&view_state_user_hover, std::convert::identity);

    let view_state_asst_hover = ConversationViewState {
        hovered_message_id: Some("m2"),
        ..view_state_unhovered
    };
    let _el3: Element<'_, ConversationMessage> =
        conversation_view(&view_state_asst_hover, std::convert::identity);
}

#[test]
fn selected_variant_content_is_displayed_after_reload() {
    let mut message = ChatMessage {
        id: "message".into(),
        author_type: "character".into(),
        author_id: Some("assistant".into()),
        content: "original".into(),
        parent_id: None,
        selected_variant_id: Some("retry".into()),
        character_image_id: Some("happy".into()),
        created_at: String::new(),
        revision: 2,
        variants: vec![Variant {
            id: "retry".into(),
            content: "replacement".into(),
            created_at: String::new(),
            revision: 2,
        }],
    };
    assert_eq!(displayed_message_content(&message), "replacement");

    message.selected_variant_id = Some("missing".into());
    assert_eq!(displayed_message_content(&message), "original");
}

mod context_usage {
    use super::super::context_usage::{description, fraction};
    use chatty_protocol::GenerationContext;

    #[test]
    fn context_tooltip_distinguishes_unknown_estimates_and_reported_counts() {
        assert!(description(None).contains("after you send"));
        let mut context = GenerationContext {
            conversation_id: "chat".into(),
            prompt_tokens: 2000,
            completion_tokens: 500,
            context_tokens: 10000,
            estimated: true,
        };
        assert_eq!(fraction(&context), Some(0.25));
        let tooltip = description(Some(&context));
        assert_eq!(tooltip, "Context used: ~2,500 / 10,000 tokens (25.0%)");
        context.estimated = false;
        assert_eq!(
            description(Some(&context)),
            "Context used: 2,500 / 10,000 tokens (25.0%)"
        );
        context.prompt_tokens = 12000;
        assert_eq!(fraction(&context), Some(1.25));
        assert!(description(Some(&context)).contains("125.0%"));
        context.context_tokens = 0;
        assert_eq!(fraction(&context), None);
        assert!(description(Some(&context)).contains("after you send"));
    }
}
