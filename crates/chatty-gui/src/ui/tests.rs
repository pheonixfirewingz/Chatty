use super::*;
use iced::widget::button;
use iced::widget::markdown;
use iced::{Background, Color, Element, Theme};

#[test]
fn glass_text_contrast_survives_extreme_desktops() {
    fn luminance(color: Color) -> f32 {
        fn linear(value: f32) -> f32 {
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }
    fn check(background: Color, text: Color) {
        let a = luminance(background);
        let b = luminance(text);
        let ratio = (a.max(b) + 0.05) / (a.min(b) + 0.05);
        assert!(ratio >= 4.5, "text contrast {ratio} on {background:?}");
    }
    for desktop in [Color::BLACK, Color::WHITE] {
        let base = composite(desktop, surface_color(true));
        for background in [
            raised_background(true),
            sidebar_panel_style(true).background.unwrap(),
        ] {
            let Background::Color(tint) = background else {
                panic!("expected uniform glass tint");
            };
            let composed = composite(base, tint);
            check(composed, PALETTE.text);
            check(composed, SECONDARY_TEXT);
            check(composed, ACCENT_TEXT);
        }
    }
}

#[test]
fn test_color_constants() {
    assert_eq!(COLOR_PRIMARY, Color::from_rgb8(0x63, 0x66, 0xf1));
    assert_eq!(COLOR_PRIMARY_STRONG, Color::from_rgb8(0x4f, 0x46, 0xe5));
    assert_eq!(COLOR_ON_PRIMARY, Color::WHITE);
}

#[test]
fn test_theme_palette() {
    assert_eq!(PALETTE.primary, COLOR_PRIMARY);
    assert_eq!(theme().palette(), PALETTE);
}

#[test]
fn test_surface_color_math() {
    // The window ships with a clear base pane; text gets separate backing.
    let glass = surface_color(true);
    assert!((glass.a - glass_alpha(TRANSPARENCY)).abs() < 1e-6);
    assert!((glass.a - 0.52).abs() < 1e-4);
    assert!(glass.r < 0.1);

    // Transparency 0: the base is 66% opaque.
    assert!((glass_alpha(0.0) - 0.66).abs() < 1e-4);

    // Transparency 20: alpha 0.58.
    assert!((glass_alpha(20.0) - 0.58).abs() < 1e-4);

    // Transparency 80: alpha 0.34, with readable content plates above it.
    assert!((glass_alpha(80.0) - 0.34).abs() < 1e-4);

    // Transparency > 80 is clamped to 80
    assert_eq!(glass_alpha(100.0), glass_alpha(80.0));

    // Non-glass mode ignores the frost: alpha 1.0
    assert_eq!(surface_color(false).a, 1.0);

    // The frost never turns fully clear
    assert!(glass_alpha(80.0) > 0.3);
}

#[test]
fn test_composite_and_glass_background() {
    // Source-over of an opaque white over an opaque base
    let over_white = composite(Color::BLACK, Color::WHITE);
    assert_eq!(over_white, Color::WHITE);

    // A fully transparent highlight leaves the base untouched
    let untouched = composite(Color::BLACK, Color::TRANSPARENT);
    assert_eq!(untouched, Color::BLACK);

    // Blending keeps the result opaque when the base is opaque
    let mixed = composite(
        Color::from_rgb(0.2, 0.2, 0.2),
        Color::from_rgba(1.0, 1.0, 1.0, 0.5),
    );
    assert!((mixed.a - 1.0).abs() < 1e-6);
    assert!(mixed.r > 0.5);

    let tint = Color::from_rgba(0.1, 0.1, 0.1, 0.8);
    assert_eq!(glass_background(tint), Background::Color(tint));
}

#[test]
fn test_surface_raised_and_border_colors() {
    let raised_glass = surface_raised_color(true);
    assert!((raised_glass.a - (238.0 / 255.0)).abs() < 1e-3);

    // Raised plates stay readable over the frosted pane behind them
    assert!(raised_glass.a > surface_color(true).a);

    let border_glass = border_color(true);
    assert!((border_glass.a - (87.0 / 255.0)).abs() < 1e-3);

    let border_opaque = border_color(false);
    assert_eq!(border_opaque.a, 1.0);
}

#[test]
fn test_container_styles() {
    let card = card_style(false);
    assert!(card.background.is_some());
    assert_eq!(card.border.radius.top_left, 14.0);

    let modal = modal_dialog_frame_style(true);
    assert!(modal.background.is_some());
    assert_eq!(modal.border.radius.top_left, 20.0);

    // Popups must stay readable: the frame itself is nearly opaque glass
    let Some(Background::Color(tint)) = modal.background else {
        panic!("modal frame must use a uniform tint");
    };
    assert!(tint.a > 0.9);

    let user_bubble = user_chat_bubble_style();
    assert_eq!(user_bubble.text_color, Some(COLOR_ON_PRIMARY));
    assert_eq!(user_bubble.border.radius.top_left, 18.0);
    assert!(matches!(user_bubble.background, Some(Background::Color(_))));

    let assistant_bubble = assistant_chat_bubble_style(false);
    assert_eq!(assistant_bubble.border.radius.top_left, 18.0);

    let sidebar = sidebar_panel_style(true);
    assert_eq!(sidebar.border.radius.top_left, 18.0);
    assert!(matches!(sidebar.background, Some(Background::Color(_))));

    let window = window_backdrop_style(true);
    assert!(window.background.is_some());

    // Navigation has stronger backing than the clear window pane.
    let sidebar_dark = sidebar_panel_style(true);
    let Some(Background::Color(tint)) = sidebar_dark.background else {
        panic!("sidebar must use a uniform tint");
    };
    assert!(tint.a > surface_color(true).a);
}

#[test]
fn test_tooltip_styles() {
    let tooltip = tooltip_style(&theme());
    assert!(tooltip.background.is_some());
    assert!(tooltip.shadow.blur_radius > 0.0);
}

#[test]
fn test_icon_types() {
    let icons = [
        FooterIcon::Characters,
        FooterIcon::World,
        FooterIcon::Memory,
        FooterIcon::Admin,
        FooterIcon::Settings,
        FooterIcon::SignOut,
    ];
    for icon in icons {
        let cloned = icon;
        assert_eq!(icon, cloned);
        let vec_icon = VectorIcon::new(icon);
        assert_eq!(vec_icon.kind, IconKind::Footer(icon));
    }

    let toggles = [SidebarToggle::Collapse, SidebarToggle::Expand];
    for toggle in toggles {
        let cloned = toggle;
        assert_eq!(toggle, cloned);
        let vec_toggle = VectorIcon::new(toggle);
        assert_eq!(vec_toggle.kind, IconKind::Sidebar(toggle));
    }

    assert_eq!(
        SidebarToggle::from_visibility(true),
        SidebarToggle::Collapse
    );
    assert_eq!(SidebarToggle::from_visibility(false), SidebarToggle::Expand);
}

#[test]
#[ignore = "manual resource measurement"]
fn markdown_rebuild_resource_probe() {
    let sample = "A **bold** paragraph with a [link](https://example.invalid).\n\n".repeat(100);
    let started = std::time::Instant::now();
    let mut markdown = Markdown::default();
    for _ in 0..500 {
        markdown.update(&sample);
        let element: Element<'_, String> = render_markdown(&markdown, |uri| uri.to_string());
        std::hint::black_box(element);
    }
    eprintln!("markdown 500 rebuilds: {:?}", started.elapsed());
    eprintln!(
        "{}",
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("VmRSS:"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn markdown_reuses_unchanged_parsing_and_releases_replaced_content() {
    let mut parsed = Markdown::default();
    parsed.update("First **reply**");
    let markdown::Item::Paragraph(paragraph) = &parsed.items[0] else {
        panic!("expected paragraph");
    };
    let spans = paragraph.spans(markdown::Style::from_palette(Theme::Dark.palette()));
    let weak = std::sync::Arc::downgrade(&spans);
    drop(spans);
    let items = parsed.items.as_ptr();
    for _ in 0..100 {
        parsed.update("First **reply**");
        let element: Element<'_, ()> = render_markdown(&parsed, |_| ());
        drop(element);
    }
    assert_eq!(items, parsed.items.as_ptr());
    assert!(weak.upgrade().is_some());
    parsed.update("Selected variant");
    assert!(weak.upgrade().is_none());
    assert_eq!(parsed.source, "Selected variant");
    parsed.update("");
    assert!(parsed.items.is_empty());
    assert!(parsed.source.is_empty());
}

#[test]
fn test_markdown_rendering() {
    let sample = "# Header\n\nThis is a **bold** paragraph with a [link](https://example.invalid).\n\n```rust\nlet x = 42;\n```\n";
    let mut markdown = Markdown::default();
    markdown.update(sample);
    let _element: Element<'_, String> = render_markdown(&markdown, |uri| uri.to_string());

    // Test empty string
    let empty = Markdown::default();
    let _empty: Element<'_, ()> = render_markdown(&empty, |_| ());
}

#[test]
fn test_toggle_switch_helper() {
    let _element: Element<'_, bool> = toggle_switch(true, "Test Switch", |state| state);
    let _element_no_label: Element<'_, bool> = toggle_switch(false, None, |state| state);
}

#[test]
fn test_icon_button_helpers() {
    let _toggle_btn: Element<'_, ()> = sidebar_toggle_button(true, ());
    let _ghost_btn: Element<'_, ()> = ghost_icon_button(SidebarToggle::Expand, Some("Expand"), ());
}

#[test]
fn test_button_styles() {
    let dummy_theme = theme();

    let primary = primary_button_style()(&dummy_theme, button::Status::Active);
    assert!(primary.background.is_some());

    let secondary = secondary_button_style()(&dummy_theme, button::Status::Active);
    assert!(secondary.background.is_some());

    let danger = danger_button_style()(&dummy_theme, button::Status::Active);
    assert!(danger.background.is_some());

    let chat_action = chat_action_button_style(false)(&dummy_theme, button::Status::Active);
    assert!(chat_action.background.is_none());
    let chat_action_hovered =
        chat_action_button_style(false)(&dummy_theme, button::Status::Hovered);
    assert!(chat_action_hovered.background.is_some());

    let chat_action_accent = chat_action_button_style(true)(&dummy_theme, button::Status::Active);
    assert!(chat_action_accent.background.is_none());
    let chat_action_accent_hovered =
        chat_action_button_style(true)(&dummy_theme, button::Status::Hovered);
    assert!(chat_action_accent_hovered.background.is_some());
}
