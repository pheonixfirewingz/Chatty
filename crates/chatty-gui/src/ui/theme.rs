//! Palette, glass surfaces, and container styles.

use iced::border::Border;
use iced::widget::container;
use iced::{Background, Color, Element, Shadow, Theme, Vector};

/// Primary accent color (#6366f1)
pub const COLOR_PRIMARY: Color = Color::from_rgb(
    0x63 as f32 / 255.0,
    0x66 as f32 / 255.0,
    0xf1 as f32 / 255.0,
);

/// Strong primary accent color for active states/buttons (#4f46e5)
pub const COLOR_PRIMARY_STRONG: Color = Color::from_rgb(
    0x4f as f32 / 255.0,
    0x46 as f32 / 255.0,
    0xe5 as f32 / 255.0,
);

/// Contrasting text/icon color on primary backgrounds (#ffffff)
pub const COLOR_ON_PRIMARY: Color = Color::WHITE;

/// Theme palette: Chatty has a single dark glass appearance.
pub const PALETTE: iced::theme::Palette = iced::theme::Palette {
    background: Color::from_rgb(
        0x0b as f32 / 255.0,
        0x0f as f32 / 255.0,
        0x17 as f32 / 255.0,
    ),
    text: Color::from_rgb(0.93, 0.94, 0.96),
    primary: COLOR_PRIMARY,
    success: Color::from_rgb(0.13, 0.77, 0.37),
    warning: Color::from_rgb(0.96, 0.62, 0.04),
    danger: Color::from_rgb(0.94, 0.27, 0.27),
};

/// Returns the Chatty theme.
/// Bold face of the default font.
pub const BOLD: iced::Font = iced::Font {
    weight: iced::font::Weight::Bold,
    ..iced::Font::DEFAULT
};

/// Semibold face of the default font.
pub const SEMIBOLD: iced::Font = iced::Font {
    weight: iced::font::Weight::Semibold,
    ..iced::Font::DEFAULT
};

pub fn theme() -> Theme {
    Theme::custom("chatty-dark", PALETTE)
}

/// Hard-coded window transparency (0..=80), fixed so every install shows the
/// same frost. There is no user control for it by design.
pub const TRANSPARENCY: f32 = 35.0;

/// Opacity of the base window glass for a transparency setting (0..=80).
///
/// The base pane leaves the desktop visible. Text surfaces add their own
/// stronger backing instead of making the whole window nearly opaque.
#[must_use]
pub fn glass_alpha(transparency: f32) -> f32 {
    let t = transparency.clamp(0.0, 80.0) / 80.0;
    0.66 - 0.32 * t
}

/// Composites `top` over `base` using the source-over operator.
#[must_use]
#[cfg(test)]
pub fn composite(base: Color, top: Color) -> Color {
    let out_a = top.a + base.a * (1.0 - top.a);
    if out_a <= f32::EPSILON {
        return Color::TRANSPARENT;
    }
    Color {
        r: (top.r * top.a + base.r * base.a * (1.0 - top.a)) / out_a,
        g: (top.g * top.a + base.g * base.a * (1.0 - top.a)) / out_a,
        b: (top.b * top.a + base.b * base.a * (1.0 - top.a)) / out_a,
        a: out_a,
    }
}

/// Uniform translucent tint without decorative gradients or texture.
#[must_use]
pub fn glass_background(base: Color) -> Background {
    Background::Color(base)
}

/// Calculates the surface background color based on glass mode.
#[must_use]
pub fn surface_color(glass_mode: bool) -> Color {
    if glass_mode {
        Color::from_rgba(
            10.0 / 255.0,
            16.0 / 255.0,
            29.0 / 255.0,
            glass_alpha(TRANSPARENCY),
        )
    } else {
        Color::from_rgb(11.0 / 255.0, 15.0 / 255.0, 23.0 / 255.0)
    }
}

/// Secondary text stays readable on both raised plates and navigation glass.
pub const SECONDARY_TEXT: Color = Color::from_rgb(0.77, 0.81, 0.88);

/// Accent for small text on glass materials.
pub const ACCENT_TEXT: Color = Color::from_rgb(0.73, 0.76, 1.0);

/// Base pane of glass: the window's own background, with a uniform tint.
#[must_use]
pub fn surface_background(glass_mode: bool) -> Background {
    glass_background(surface_color(glass_mode))
}

/// Overlay tint for a panel resting on the window glass. `strength` is how much
/// the panel frosts over the pane behind it; solid mode is fully opaque.
#[must_use]
pub fn panel_tint(glass_mode: bool, strength: f32) -> Color {
    if glass_mode {
        Color::from_rgba(
            6.0 / 255.0,
            11.0 / 255.0,
            22.0 / 255.0,
            strength.clamp(0.0, 1.0),
        )
    } else {
        Color::from_rgb(11.0 / 255.0, 15.0 / 255.0, 23.0 / 255.0)
    }
}

/// Background for the conversation column: a lighter frosting than the sidebar
/// so desktop detail remains visible between the readable message plates.
#[must_use]
pub fn chat_panel_background(glass_mode: bool) -> Background {
    let strength = 0.0;
    glass_background(panel_tint(glass_mode, strength))
}

/// Calculates the raised surface background color (cards, dialogs, composer).
#[must_use]
pub fn surface_raised_color(glass_mode: bool) -> Color {
    if glass_mode {
        Color::from_rgba(18.0 / 255.0, 26.0 / 255.0, 44.0 / 255.0, 238.0 / 255.0)
    } else {
        Color::from_rgb(24.0 / 255.0, 32.0 / 255.0, 46.0 / 255.0)
    }
}

/// Raised plate background: cards, bubbles and dialogs with a uniform tint.
#[must_use]
pub fn raised_background(glass_mode: bool) -> Background {
    glass_background(surface_raised_color(glass_mode))
}

/// Calculates the border color for cards, panels, and frames.
///
/// In glass mode this is the rim light: a bright hairline catching the edge of
/// the pane.
#[must_use]
pub fn border_color(glass_mode: bool) -> Color {
    if glass_mode {
        Color::from_rgba(199.0 / 255.0, 214.0 / 255.0, 1.0, 87.0 / 255.0)
    } else {
        Color::from_rgb(38.0 / 255.0, 48.0 / 255.0, 68.0 / 255.0)
    }
}

/// Rounded input plate with a visible keyboard focus ring.
#[must_use]
pub fn input_style(
    theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let mut style = iced::widget::text_input::default(theme, status);
    style.border.radius = 12.0.into();
    let focused = matches!(status, iced::widget::text_input::Status::Focused { .. });
    style.border.width = if focused { 1.0 } else { 0.0 };
    style.border.color = if focused {
        ACCENT_TEXT
    } else {
        Color::TRANSPARENT
    };
    style.placeholder = SECONDARY_TEXT;
    style
}

/// Multi-line counterpart of [`input_style`].
#[must_use]
pub fn editor_style(
    theme: &Theme,
    status: iced::widget::text_editor::Status,
) -> iced::widget::text_editor::Style {
    use iced::widget::{text_editor, text_input};
    let input = input_style(
        theme,
        match status {
            text_editor::Status::Active => text_input::Status::Active,
            text_editor::Status::Hovered => text_input::Status::Hovered,
            text_editor::Status::Focused { is_hovered } => {
                text_input::Status::Focused { is_hovered }
            }
            text_editor::Status::Disabled => text_input::Status::Disabled,
        },
    );
    text_editor::Style {
        background: input.background,
        border: input.border,
        placeholder: input.placeholder,
        value: input.value,
        selection: input.selection,
    }
}

/// Container style for the root window: the pane of glass the app is etched from.
#[must_use]
pub fn window_backdrop_style(glass_mode: bool) -> container::Style {
    container::Style {
        background: Some(surface_background(glass_mode)),
        text_color: None,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// Container style for cards.
#[must_use]
pub fn card_style(glass_mode: bool) -> container::Style {
    container::Style {
        background: Some(raised_background(glass_mode)),
        text_color: None,
        border: Border {
            color: border_color(glass_mode),
            width: 0.0,
            radius: 14.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.42),
            offset: Vector::new(0.0, 6.0),
            blur_radius: 18.0,
        },
        snap: false,
    }
}

/// Container style for modal dialog frames.
///
/// Popups sit closest to the viewer, so they are almost fully cured glass:
/// opaque enough to read against without a visible outline.
#[must_use]
pub fn modal_dialog_frame_style(glass_mode: bool) -> container::Style {
    let bg = if glass_mode {
        Color::from_rgba(14.0 / 255.0, 20.0 / 255.0, 34.0 / 255.0, 246.0 / 255.0)
    } else {
        Color::from_rgb(17.0 / 255.0, 23.0 / 255.0, 34.0 / 255.0)
    };

    container::Style {
        background: Some(glass_background(bg)),
        text_color: None,
        border: Border {
            color: border_color(glass_mode),
            width: 0.0,
            radius: 20.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.62),
            offset: Vector::new(0.0, 24.0),
            blur_radius: 64.0,
        },
        snap: false,
    }
}

/// Frosted tooltip/popover styling. Tooltips are always glass, whichever
/// surface mode the rest of the app is in.
#[must_use]
pub fn tooltip_style(theme: &Theme) -> container::Style {
    let base = Color::from_rgba(12.0 / 255.0, 17.0 / 255.0, 28.0 / 255.0, 245.0 / 255.0);
    container::Style {
        background: Some(glass_background(base)),
        text_color: Some(theme.palette().text),
        border: Border {
            color: border_color(true),
            width: 0.0,
            radius: 10.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.50),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 24.0,
        },
        snap: false,
    }
}

/// Container style for user chat bubbles.
#[must_use]
pub fn user_chat_bubble_style() -> container::Style {
    let base = Color::from_rgba8(0x4f, 0x46, 0xe5, 242.0 / 255.0 * 0.9);
    container::Style {
        background: Some(glass_background(base)),
        text_color: Some(COLOR_ON_PRIMARY),
        border: Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.28),
            width: 0.0,
            radius: 18.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.12),
            offset: Vector::new(0.0, 4.0),
            blur_radius: 14.0,
        },
        snap: false,
    }
}

/// Container style for assistant chat bubbles.
#[must_use]
pub fn assistant_chat_bubble_style(glass_mode: bool) -> container::Style {
    let mut base = surface_raised_color(glass_mode);
    base.a *= 0.9;
    container::Style {
        background: Some(glass_background(base)),
        text_color: None,
        border: Border {
            color: border_color(glass_mode),
            width: 0.0,
            radius: 18.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.34),
            offset: Vector::new(0.0, 3.0),
            blur_radius: 12.0,
        },
        snap: false,
    }
}

/// Container style for sidebar panels.
#[must_use]
pub fn sidebar_panel_style(glass_mode: bool) -> container::Style {
    let base = if glass_mode {
        panel_tint(glass_mode, 0.76)
    } else {
        surface_color(glass_mode)
    };
    container::Style {
        background: Some(glass_background(base)),
        text_color: None,
        border: Border {
            color: border_color(glass_mode),
            width: 0.0,
            radius: 18.0.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.38),
            offset: Vector::new(4.0, 0.0),
            blur_radius: 24.0,
        },
        snap: false,
    }
}

/// Creates a card container with standard padding and rounded border.
pub fn card<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    glass_mode: bool,
) -> container::Container<'a, Message> {
    container(content)
        .padding(12)
        .style(move |_| card_style(glass_mode))
}

/// Creates a styled sidebar panel container.
pub fn sidebar_panel<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    glass_mode: bool,
) -> container::Container<'a, Message> {
    container(content)
        .padding(12)
        .style(move |_| sidebar_panel_style(glass_mode))
}
