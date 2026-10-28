use super::Harness;
use crate::app::view::sidebar_icon_layout;
use crate::app::{Message, Screen};
use crate::conversation::{ConversationMarkdown, ConversationMessage};
use iced::Size;

#[test]
fn sidebar_icons_shrink_before_wrapping_into_threes() {
    for count in [5, 6] {
        let (size, columns) = sidebar_icon_layout(300.0, count);
        assert_eq!((size, columns), (44.0, count));

        let threshold = count as f32 * 32.0 + (count - 1) as f32 * 6.0;
        let (size, columns) = sidebar_icon_layout(threshold, count);
        assert_eq!((size, columns), (32.0, count));
        let (size, columns) = sidebar_icon_layout(threshold - 1.0, count);
        assert_eq!(columns, 3);
        assert!((32.0..=44.0).contains(&size));
    }

    let (size, columns) = sidebar_icon_layout(224.0, 6);
    assert_eq!(columns, 6);
    assert!(size >= 32.0);
    assert!(size * 6.0 + 30.0 <= 224.0);
}

#[test]
fn signed_in_login_state_keeps_markdown_when_composer_changes() {
    let mut app = Harness::demo();
    app.connected = true;
    // Authentication controls the visible shell independently of Screen.
    assert_eq!(app.screen, Screen::Login);
    app.update(ConversationMessage::InputChanged("hello".into()));
    let _ = app.view_at_size(Size::new(800.0, 720.0));
    app.update(Message::SignOut);
    assert_eq!(
        format!("{:?}", app.markdown),
        format!("{:?}", ConversationMarkdown::default())
    );
}

#[test]
fn view_builds_without_panics_for_all_states() {
    let small = Size::new(360.0, 300.0);
    let wide = Size::new(1280.0, 800.0);
    let mut app = Harness::new();
    let _ = app.view_at_size(small);
    app.connected = true;
    let _ = app.view_at_size(small);

    let mut app = Harness::demo();
    app.connected = true;
    app.screen = Screen::Chat;
    type Toggle = fn(&mut Harness, bool);
    let dialogs: [Toggle; 4] = [
        |app, open| app.new_chat_open = open,
        |app, open| app.draft_character_open = open,
        |app, open| app.worlds_open = open,
        |app, open| app.memory_dialog_open = open,
    ];
    for size in [small, wide] {
        let _ = app.view_at_size(size);
        for toggle in dialogs {
            toggle(&mut app, true);
            let _ = app.view_at_size(size);
            toggle(&mut app, false);
        }
        for screen in [Screen::Admin, Screen::Settings, Screen::Chat] {
            app.screen = screen;
            let _ = app.view_at_size(size);
        }
        app.set_error("Notice");
        let _ = app.view_at_size(size);
        app.error = None;
    }
}
