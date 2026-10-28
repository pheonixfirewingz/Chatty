//! Icon buttons, markdown, toggles, and button styles.

use super::*;
use iced::border::Border;
use iced::widget::canvas::{self};
use iced::widget::{button, container, markdown, text, toggler, tooltip};
use iced::{Background, Color, Element, Length, Padding, Renderer, Shadow, Theme, Vector};

/// Creates an icon button with a proportionally sized canvas.
pub fn sized_icon_button<'a, Message: Clone + 'a>(
    program: impl canvas::Program<Message, Theme, Renderer> + 'a,
    tooltip_text: Option<&'static str>,
    on_press: Message,
    size: f32,
) -> Element<'a, Message> {
    let icon_canvas = canvas::Canvas::new(program)
        .width(size * 28.0 / 44.0)
        .height(size * 28.0 / 44.0);

    let btn = button(
        container(icon_canvas)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(size)
    .height(size)
    .padding(0)
    .on_press(on_press)
    .style(|theme: &Theme, status: button::Status| {
        let palette = theme.palette();
        let (bg, border_color) = match status {
            button::Status::Active => (
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.06),
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.12),
            ),
            button::Status::Hovered => (
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.14),
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.24),
            ),
            button::Status::Pressed => (
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.22),
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.32),
            ),
            button::Status::Disabled => (Color::TRANSPARENT, Color::TRANSPARENT),
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: palette.text,
            border: Border {
                color: border_color,
                width: 0.0,
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        }
    });

    if let Some(label) = tooltip_text {
        tooltip(
            btn,
            container(text(label).size(12))
                .padding(Padding::from([4, 8]))
                .style(tooltip_style),
            tooltip::Position::Top,
        )
        .into()
    } else {
        btn.into()
    }
}

/// Fill style of an [`action_icon`] button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Secondary,
    Danger,
}

/// Compact icon-only action button; the tooltip names the action.
/// `None` disables the button and dims its icon.
pub fn action_icon<'a, Message: Clone + 'a>(
    icon: impl Into<IconKind>,
    label: impl Into<String>,
    on_press: Option<Message>,
    tone: Tone,
) -> Element<'a, Message> {
    let mut color = match tone {
        Tone::Secondary => PALETTE.text,
        Tone::Primary | Tone::Danger => Color::WHITE,
    };
    if on_press.is_none() {
        color.a = 0.4;
    }
    let icon = canvas::Canvas::new(VectorIcon::new(icon).with_color(color))
        .width(18)
        .height(18);
    let control = button(icon)
        .width(32)
        .height(32)
        .padding(7)
        .on_press_maybe(on_press)
        .style(move |theme: &Theme, status| match tone {
            Tone::Primary => primary_button_style()(theme, status),
            Tone::Secondary => secondary_button_style()(theme, status),
            Tone::Danger => danger_button_style()(theme, status),
        });
    tooltip(
        control,
        container(text(label.into()).size(12))
            .padding(Padding::from([4, 8]))
            .style(tooltip_style),
        tooltip::Position::Top,
    )
    .into()
}

/// Creates a styled ghost icon button with optional tooltip.
pub fn ghost_icon_button<'a, Message: Clone + 'a>(
    program: impl canvas::Program<Message, Theme, Renderer> + 'a,
    tooltip_text: Option<&'static str>,
    on_press: Message,
) -> Element<'a, Message> {
    let icon_canvas = canvas::Canvas::new(program).width(24).height(24);

    let btn = button(
        container(icon_canvas)
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(38)
    .height(38)
    .padding(0)
    .on_press(on_press)
    .style(|theme: &Theme, status: button::Status| {
        let palette = theme.palette();
        let (bg, border_color) = match status {
            button::Status::Active => (Color::TRANSPARENT, Color::TRANSPARENT),
            button::Status::Hovered => (
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.10),
                Color::TRANSPARENT,
            ),
            button::Status::Pressed => (
                Color::from_rgba(palette.text.r, palette.text.g, palette.text.b, 0.18),
                Color::TRANSPARENT,
            ),
            button::Status::Disabled => (Color::TRANSPARENT, Color::TRANSPARENT),
        };
        button::Style {
            background: if bg == Color::TRANSPARENT {
                None
            } else {
                Some(Background::Color(bg))
            },
            text_color: palette.text,
            border: Border {
                color: border_color,
                width: 0.0,
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        }
    });

    if let Some(label) = tooltip_text {
        tooltip(
            btn,
            container(text(label).size(12))
                .padding(Padding::from([4, 8]))
                .style(tooltip_style),
            tooltip::Position::Top,
        )
        .into()
    } else {
        btn.into()
    }
}

/// Creates a sidebar toggle collapse/expand ghost button.
pub fn sidebar_toggle_button<'a, Message: Clone + 'a>(
    sidebar_visible: bool,
    on_press: Message,
) -> Element<'a, Message> {
    let icon = SidebarToggle::from_visibility(sidebar_visible);
    let label = if sidebar_visible {
        "Hide sidebar"
    } else {
        "Show sidebar"
    };
    ghost_icon_button(icon, Some(label), on_press)
}

/// Parsed markdown owned by the loaded conversation, never by a global cache.
#[derive(Debug, Clone, Default)]
pub struct Markdown {
    pub(super) source: String,
    pub(super) items: Vec<markdown::Item>,
}

impl Markdown {
    /// Reparse only when displayed content changes, including variant switches.
    pub fn update(&mut self, text: &str) {
        if self.source != text {
            *self = Self {
                source: text.to_owned(),
                items: markdown::parse(text).collect(),
            };
        }
    }
}

/// Borrows parsed markdown for the lifetime of the rendered element.
pub fn render_markdown<'a, Message: Clone + 'a>(
    markdown: &'a Markdown,
    on_link: impl Fn(markdown::Uri) -> Message + 'a,
) -> Element<'a, Message> {
    render_markdown_items(&markdown.items, on_link)
}

/// Renders pre-parsed markdown items into an Iced Element.
pub fn render_markdown_items<'a, Message: Clone + 'a>(
    items: &'a [markdown::Item],
    on_link: impl Fn(markdown::Uri) -> Message + 'a,
) -> Element<'a, Message> {
    markdown::view(items, &Theme::Dark).map(on_link)
}

/// Consistent styled toggle switch widget.
pub fn toggle_switch<'a, Message: Clone + 'a>(
    is_toggled: bool,
    label: impl Into<Option<&'a str>>,
    on_toggle: impl Fn(bool) -> Message + 'a,
) -> Element<'a, Message> {
    let mut t = toggler(is_toggled)
        .on_toggle(on_toggle)
        .size(20.0)
        .spacing(8.0)
        .style(|_theme: &Theme, status: toggler::Status| {
            let is_toggled = match status {
                toggler::Status::Active { is_toggled }
                | toggler::Status::Hovered { is_toggled }
                | toggler::Status::Disabled { is_toggled } => is_toggled,
            };
            let (bg, fg) = if is_toggled {
                (COLOR_PRIMARY_STRONG, COLOR_ON_PRIMARY)
            } else {
                (
                    Color::from_rgb(100.0 / 255.0, 116.0 / 255.0, 139.0 / 255.0),
                    Color::from_rgb(241.0 / 255.0, 245.0 / 255.0, 249.0 / 255.0),
                )
            };
            toggler::Style {
                background: Background::Color(bg),
                background_border_width: 0.0,
                background_border_color: Color::TRANSPARENT,
                foreground: Background::Color(fg),
                foreground_border_width: 0.0,
                foreground_border_color: Color::TRANSPARENT,
                text_color: None,
                border_radius: None,
                padding_ratio: 0.15,
            }
        });

    if let Some(lbl) = label.into() {
        t = t.label(lbl);
    }

    t.into()
}

/// Style for primary action buttons (Send, Continue, Create, New Chat, etc.).
pub fn primary_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, shadow_color) = match status {
            button::Status::Active => (COLOR_PRIMARY_STRONG, Color::from_rgba(0.0, 0.0, 0.0, 0.18)),
            button::Status::Hovered => (COLOR_PRIMARY, Color::from_rgba(0.0, 0.0, 0.0, 0.28)),
            button::Status::Pressed => (
                Color::from_rgb(0.25, 0.22, 0.75),
                Color::from_rgba(0.0, 0.0, 0.0, 0.10),
            ),
            button::Status::Disabled => (Color::from_rgba(0.4, 0.4, 0.5, 0.3), Color::TRANSPARENT),
        };

        button::Style {
            background: Some(Background::Color(bg)),
            text_color: Color::WHITE,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: 10.0.into(),
            },
            shadow: Shadow {
                color: shadow_color,
                offset: Vector::new(0.0, 2.0),
                blur_radius: 4.0,
            },
            snap: false,
        }
    }
}

/// Style for secondary / standard dialog buttons (Cancel, Close, etc.).
pub fn secondary_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, border_col, text_col) = match status {
            button::Status::Active => (
                Color::from_rgba(1.0, 1.0, 1.0, 0.08),
                Color::from_rgba(1.0, 1.0, 1.0, 0.16),
                Color::from_rgb(0.9, 0.9, 0.95),
            ),
            button::Status::Hovered => (
                Color::from_rgba(1.0, 1.0, 1.0, 0.14),
                Color::from_rgba(1.0, 1.0, 1.0, 0.26),
                Color::WHITE,
            ),
            button::Status::Pressed => (
                Color::from_rgba(1.0, 1.0, 1.0, 0.20),
                Color::from_rgba(1.0, 1.0, 1.0, 0.35),
                Color::WHITE,
            ),
            button::Status::Disabled => (
                Color::TRANSPARENT,
                Color::from_rgba(0.5, 0.5, 0.5, 0.2),
                Color::from_rgba(0.5, 0.5, 0.5, 0.4),
            ),
        };

        button::Style {
            background: Some(Background::Color(bg)),
            text_color: text_col,
            border: Border {
                color: border_col,
                width: 0.0,
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// Style for danger action buttons (Stop, Delete).
pub fn danger_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, border_col) = match status {
            button::Status::Active => (
                Color::from_rgb(0.85, 0.22, 0.22),
                Color::from_rgb(0.95, 0.30, 0.30),
            ),
            button::Status::Hovered => (
                Color::from_rgb(0.95, 0.28, 0.28),
                Color::from_rgb(1.0, 0.40, 0.40),
            ),
            button::Status::Pressed => (
                Color::from_rgb(0.70, 0.18, 0.18),
                Color::from_rgb(0.80, 0.22, 0.22),
            ),
            button::Status::Disabled => (Color::from_rgba(0.8, 0.2, 0.2, 0.3), Color::TRANSPARENT),
        };

        button::Style {
            background: Some(Background::Color(bg)),
            text_color: Color::WHITE,
            border: Border {
                color: border_col,
                width: 0.0,
                radius: 8.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// Style for ghost buttons: frameless and transparent in resting state, subtle highlight on hover.
pub fn ghost_button_style(on_accent: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, text_col) = if on_accent {
            match status {
                button::Status::Active => {
                    (Color::TRANSPARENT, Color::from_rgba(1.0, 1.0, 1.0, 0.85))
                }
                button::Status::Hovered => (Color::from_rgba(1.0, 1.0, 1.0, 0.22), Color::WHITE),
                button::Status::Pressed => (Color::from_rgba(1.0, 1.0, 1.0, 0.35), Color::WHITE),
                button::Status::Disabled => {
                    (Color::TRANSPARENT, Color::from_rgba(1.0, 1.0, 1.0, 0.40))
                }
            }
        } else {
            match status {
                button::Status::Active => (Color::TRANSPARENT, Color::from_rgb(0.80, 0.84, 0.92)),
                button::Status::Hovered => (Color::from_rgba(1.0, 1.0, 1.0, 0.12), Color::WHITE),
                button::Status::Pressed => (Color::from_rgba(1.0, 1.0, 1.0, 0.20), Color::WHITE),
                button::Status::Disabled => {
                    (Color::TRANSPARENT, Color::from_rgba(0.5, 0.5, 0.5, 0.4))
                }
            }
        };

        button::Style {
            background: if bg == Color::TRANSPARENT {
                None
            } else {
                Some(Background::Color(bg))
            },
            text_color: text_col,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: 4.0.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        }
    }
}

/// Style for action buttons in assistant and user message bubbles.
pub fn chat_action_button_style(
    on_accent: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    ghost_button_style(on_accent)
}
