use super::Harness;
use crate::app::Screen;

#[test]
fn inspection_commands_drive_screens_and_dialogs() {
    let mut app = Harness::demo();
    let _ = app.inspect_control("models 3");
    assert_eq!(app.screen, Screen::Admin);
    assert_eq!((app.admin.admin_tab, app.admin.models.page), (2, 3));

    let _ = app.inspect_control("sidebar closed");
    assert!(!app.sidebar_visible);
    let _ = app.inspect_control("sidebar");
    assert!(app.sidebar_visible);

    let _ = app.inspect_control("zoom 9");
    assert_eq!(app.zoom, 3.0);

    let _ = app.inspect_control("hover demo");
    assert_eq!(app.hovered_conversation_id.as_deref(), Some("demo"));
    let _ = app.inspect_control("hover");
    assert_eq!(app.hovered_message_id.as_deref(), Some("m2"));
    let _ = app.inspect_control("hover none");
    assert!(app.hovered_message_id.is_none() && app.hovered_conversation_id.is_none());

    let _ = app.inspect_control("worlds");
    let _ = app.inspect_control("close_dialogs");
    assert!(!app.worlds_open);
    assert_eq!(app.screen, Screen::Chat);

    let _ = app.inspect_control("unknown command");
    let _ = app.inspect_control("");
}
