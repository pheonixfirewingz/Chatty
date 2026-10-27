//! Native Iced 0.14 UI styling, theme palettes, vector canvas icons, and markdown rendering.
#![allow(dead_code)]

use iced::border::Border;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{button, container, markdown, text, toggler, tooltip};
use iced::{
    Background, Color, Element, Length, Padding, Point, Radians, Rectangle, Renderer,
    Shadow, Size, Theme, Vector,
};

// -----------------------------------------------------------------------------
// Themes and Colors
// -----------------------------------------------------------------------------

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

/// Light mode theme palette
pub const LIGHT_PALETTE: iced::theme::Palette = iced::theme::Palette {
    background: Color::from_rgb(
        0xf6 as f32 / 255.0,
        0xf7 as f32 / 255.0,
        0xfb as f32 / 255.0,
    ),
    text: Color::from_rgb(0.08, 0.10, 0.15),
    primary: COLOR_PRIMARY,
    success: Color::from_rgb(0.13, 0.77, 0.37),
    warning: Color::from_rgb(0.96, 0.62, 0.04),
    danger: Color::from_rgb(0.94, 0.27, 0.27),
};

/// Dark mode theme palette
pub const DARK_PALETTE: iced::theme::Palette = iced::theme::Palette {
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

/// Returns the palette for the given light/dark mode.
#[must_use]
pub fn palette(light_mode: bool) -> iced::theme::Palette {
    if light_mode {
        LIGHT_PALETTE
    } else {
        DARK_PALETTE
    }
}

/// Returns a Theme configured for Chatty with either Light or Dark mode.
#[must_use]
pub fn theme(light_mode: bool) -> Theme {
    Theme::custom(
        if light_mode { "chatty-light" } else { "chatty-dark" },
        palette(light_mode),
    )
}

// -----------------------------------------------------------------------------
// Liquid Glass Surfaces
// -----------------------------------------------------------------------------

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

/// Detects light mode from a theme palette, for styling sites that only carry
/// the theme (tooltips, catalog styles) and not the app state.
#[must_use]
pub fn is_light_theme(theme: &Theme) -> bool {
    let bg = theme.palette().background;
    (bg.r * 0.299 + bg.g * 0.587 + bg.b * 0.114) > 0.5
}

/// Calculates the surface background color based on light/dark mode and glass mode.
#[must_use]
pub fn surface_color(light_mode: bool, glass_mode: bool) -> Color {
    if glass_mode {
        let alpha = glass_alpha(TRANSPARENCY);

        if light_mode {
            Color::from_rgba(
                246.0 / 255.0,
                248.0 / 255.0,
                1.0,
                alpha,
            )
        } else {
            Color::from_rgba(
                10.0 / 255.0,
                16.0 / 255.0,
                29.0 / 255.0,
                alpha,
            )
        }
    } else if light_mode {
        Color::from_rgb(
            246.0 / 255.0,
            247.0 / 255.0,
            251.0 / 255.0,
        )
    } else {
        Color::from_rgb(
            11.0 / 255.0,
            15.0 / 255.0,
            23.0 / 255.0,
        )
    }
}

/// Secondary text stays readable on both raised plates and navigation glass.
#[must_use]
pub fn secondary_text_color(light_mode: bool) -> Color {
    if light_mode {
        Color::from_rgb(0.25, 0.29, 0.36)
    } else {
        Color::from_rgb(0.77, 0.81, 0.88)
    }
}

/// Theme-aware accent for small text on light and dark materials.
#[must_use]
pub fn accent_text_color(light_mode: bool) -> Color {
    if light_mode {
        COLOR_PRIMARY_STRONG
    } else {
        Color::from_rgb(0.73, 0.76, 1.0)
    }
}

/// Base pane of glass: the window's own background, with a uniform tint.
#[must_use]
pub fn surface_background(light_mode: bool, glass_mode: bool) -> Background {
    let base = surface_color(light_mode, glass_mode);
    glass_background(base)
}

/// Overlay tint for a panel resting on the window glass. `strength` is how much
/// the panel frosts over the pane behind it; solid mode is fully opaque.
#[must_use]
pub fn panel_tint(light_mode: bool, glass_mode: bool, strength: f32) -> Color {
    if glass_mode {
        let a = strength.clamp(0.0, 1.0);
        if light_mode {
            Color::from_rgba(1.0, 1.0, 1.0, a)
        } else {
            Color::from_rgba(6.0 / 255.0, 11.0 / 255.0, 22.0 / 255.0, a)
        }
    } else if light_mode {
        Color::from_rgb(246.0 / 255.0, 247.0 / 255.0, 251.0 / 255.0)
    } else {
        Color::from_rgb(11.0 / 255.0, 15.0 / 255.0, 23.0 / 255.0)
    }
}

/// Background for the conversation column: a lighter frosting than the sidebar
/// so desktop detail remains visible between the readable message plates.
#[must_use]
pub fn chat_panel_background(light_mode: bool, glass_mode: bool) -> Background {
    let strength = 0.0;
    glass_background(panel_tint(light_mode, glass_mode, strength))
}

/// Calculates the raised surface background color (cards, dialogs, composer).
#[must_use]
pub fn surface_raised_color(light_mode: bool, glass_mode: bool) -> Color {
    if glass_mode {
        if light_mode {
            Color::from_rgba(
                249.0 / 255.0,
                251.0 / 255.0,
                1.0,
                235.0 / 255.0,
            )
        } else {
            Color::from_rgba(
                18.0 / 255.0,
                26.0 / 255.0,
                44.0 / 255.0,
                238.0 / 255.0,
            )
        }
    } else if light_mode {
        Color::from_rgb(
            235.0 / 255.0,
            237.0 / 255.0,
            245.0 / 255.0,
        )
    } else {
        Color::from_rgb(
            24.0 / 255.0,
            32.0 / 255.0,
            46.0 / 255.0,
        )
    }
}

/// Raised plate background: cards, bubbles and dialogs with a uniform tint.
#[must_use]
pub fn raised_background(light_mode: bool, glass_mode: bool) -> Background {
    let base = surface_raised_color(light_mode, glass_mode);
    glass_background(base)
}

/// Calculates the border color for cards, panels, and frames.
///
/// In glass mode this is the rim light: a bright hairline catching the edge of
/// the pane.
#[must_use]
pub fn border_color(light_mode: bool, glass_mode: bool) -> Color {
    if glass_mode {
        if light_mode {
            Color::from_rgba(1.0, 1.0, 1.0, 235.0 / 255.0)
        } else {
            Color::from_rgba(
                199.0 / 255.0,
                214.0 / 255.0,
                1.0,
                87.0 / 255.0,
            )
        }
    } else if light_mode {
        Color::from_rgb(
            218.0 / 255.0,
            224.0 / 255.0,
            233.0 / 255.0,
        )
    } else {
        Color::from_rgb(
            38.0 / 255.0,
            48.0 / 255.0,
            68.0 / 255.0,
        )
    }
}

/// Rounded input plate with a visible keyboard focus ring.
#[must_use]
pub fn input_style(
    theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let light_mode = is_light_theme(theme);
    let mut style = iced::widget::text_input::default(theme, status);
    style.border.radius = 12.0.into();
    let focused = matches!(status, iced::widget::text_input::Status::Focused { .. });
    style.border.width = if focused { 1.0 } else { 0.0 };
    style.border.color = if focused {
        accent_text_color(light_mode)
    } else {
        Color::TRANSPARENT
    };
    style.placeholder = secondary_text_color(light_mode);
    style
}

// -----------------------------------------------------------------------------
// Container Styles
// -----------------------------------------------------------------------------

/// Container style for the root window: the pane of glass the app is etched from.
#[must_use]
pub fn window_backdrop_style(light_mode: bool, glass_mode: bool) -> container::Style {
    container::Style {
        background: Some(surface_background(light_mode, glass_mode)),
        text_color: None,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// Container style for cards.
#[must_use]
pub fn card_style(light_mode: bool, glass_mode: bool) -> container::Style {
    container::Style {
        background: Some(raised_background(light_mode, glass_mode)),
        text_color: None,
        border: Border {
            color: border_color(light_mode, glass_mode),
            width: 0.0,
            radius: 14.0.into(),
        },
        shadow: Shadow {
            color: if light_mode {
                Color::from_rgba(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.12)
            } else {
                Color::from_rgba(0.0, 0.0, 0.0, 0.42)
            },
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
pub fn modal_dialog_frame_style(light_mode: bool, glass_mode: bool) -> container::Style {
    let bg = if glass_mode {
        if light_mode {
            Color::from_rgba(252.0 / 255.0, 253.0 / 255.0, 1.0, 244.0 / 255.0)
        } else {
            Color::from_rgba(
                14.0 / 255.0,
                20.0 / 255.0,
                34.0 / 255.0,
                246.0 / 255.0,
            )
        }
    } else if light_mode {
        Color::WHITE
    } else {
        Color::from_rgb(
            17.0 / 255.0,
            23.0 / 255.0,
            34.0 / 255.0,
        )
    };

    container::Style {
        background: Some(glass_background(bg)),
        text_color: None,
        border: Border {
            color: border_color(light_mode, glass_mode),
            width: 0.0,
            radius: 20.0.into(),
        },
        shadow: Shadow {
            color: if light_mode {
                Color::from_rgba(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.30)
            } else {
                Color::from_rgba(0.0, 0.0, 0.0, 0.62)
            },
            offset: Vector::new(0.0, 24.0),
            blur_radius: 64.0,
        },
        snap: false,
    }
}

/// Container style for modal scrim background dimming.
#[must_use]
pub fn modal_scrim_style(light_mode: bool) -> container::Style {
    let alpha = if light_mode { 0.32 } else { 0.55 };
    container::Style {
        background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, alpha))),
        text_color: None,
        border: Border::default(),
        shadow: Shadow::default(),
        snap: false,
    }
}

/// Frosted tooltip/popover styling. Tooltips are always glass, whichever
/// surface mode the rest of the app is in.
#[must_use]
pub fn tooltip_style(theme: &Theme) -> container::Style {
    let light_mode = is_light_theme(theme);
    let base = if light_mode {
        Color::from_rgba(252.0 / 255.0, 253.0 / 255.0, 1.0, 245.0 / 255.0)
    } else {
        Color::from_rgba(12.0 / 255.0, 17.0 / 255.0, 28.0 / 255.0, 245.0 / 255.0)
    };
    container::Style {
        background: Some(glass_background(base)),
        text_color: Some(theme.palette().text),
        border: Border {
            color: border_color(light_mode, true),
            width: 0.0,
            radius: 10.0.into(),
        },
        shadow: Shadow {
            color: if light_mode {
                Color::from_rgba(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.22)
            } else {
                Color::from_rgba(0.0, 0.0, 0.0, 0.50)
            },
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
pub fn assistant_chat_bubble_style(light_mode: bool, glass_mode: bool) -> container::Style {
    let mut base = surface_raised_color(light_mode, glass_mode);
    base.a *= 0.9;
    container::Style {
        background: Some(glass_background(base)),
        text_color: None,
        border: Border {
            color: border_color(light_mode, glass_mode),
            width: 0.0,
            radius: 18.0.into(),
        },
        shadow: Shadow {
            color: if light_mode {
                Color::from_rgba(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.08)
            } else {
                Color::from_rgba(0.0, 0.0, 0.0, 0.34)
            },
            offset: Vector::new(0.0, 3.0),
            blur_radius: 12.0,
        },
        snap: false,
    }
}

/// Container style for sidebar panels.
#[must_use]
pub fn sidebar_panel_style(light_mode: bool, glass_mode: bool) -> container::Style {
    let strength = if light_mode { 0.78 } else { 0.76 };
    let base = if glass_mode {
        panel_tint(light_mode, glass_mode, strength)
    } else {
        surface_color(light_mode, glass_mode)
    };
    container::Style {
        background: Some(glass_background(base)),
        text_color: None,
        border: Border {
            color: border_color(light_mode, glass_mode),
            width: 0.0,
            radius: 18.0.into(),
        },
        shadow: Shadow {
            color: if light_mode {
                Color::from_rgba(15.0 / 255.0, 23.0 / 255.0, 42.0 / 255.0, 0.10)
            } else {
                Color::from_rgba(0.0, 0.0, 0.0, 0.38)
            },
            offset: Vector::new(4.0, 0.0),
            blur_radius: 24.0,
        },
        snap: false,
    }
}

// -----------------------------------------------------------------------------
// Container Widget Helpers
// -----------------------------------------------------------------------------

/// Creates a card container with standard padding and rounded border.
pub fn card<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    light_mode: bool,
    glass_mode: bool,
) -> container::Container<'a, Message> {
    container(content)
        .padding(12)
        .style(move |_| card_style(light_mode, glass_mode))
}

/// Creates a modal dialog frame container.
pub fn modal_dialog_frame<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    light_mode: bool,
    glass_mode: bool,
) -> container::Container<'a, Message> {
    container(content)
        .padding(16)
        .style(move |_| modal_dialog_frame_style(light_mode, glass_mode))
}

/// Creates a modal background scrim container that fills available space.
pub fn modal_scrim<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    light_mode: bool,
) -> container::Container<'a, Message> {
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(move |_| modal_scrim_style(light_mode))
}

/// Creates a styled user message chat bubble.
pub fn user_chat_bubble<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
) -> container::Container<'a, Message> {
    container(content)
        .padding(Padding::from([7, 15]))
        .style(|_| user_chat_bubble_style())
}

/// Creates a styled assistant message chat bubble.
pub fn assistant_chat_bubble<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    light_mode: bool,
    glass_mode: bool,
) -> container::Container<'a, Message> {
    container(content)
        .padding(Padding::from([7, 15]))
        .style(move |_| assistant_chat_bubble_style(light_mode, glass_mode))
}

/// Creates a styled sidebar panel container.
pub fn sidebar_panel<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    light_mode: bool,
    glass_mode: bool,
) -> container::Container<'a, Message> {
    container(content)
        .padding(12)
        .style(move |_| sidebar_panel_style(light_mode, glass_mode))
}

// -----------------------------------------------------------------------------
// Vector Icons
// -----------------------------------------------------------------------------

/// Footer navigation icons matching the original design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FooterIcon {
    /// Two heads & shoulders
    Characters,
    /// Globe with lat/long grid lines
    World,
    /// 3 stacked database cylinders / ellipses
    Memory,
    /// Shield with checkmark
    Admin,
    /// Gear wheel with teeth / spokes
    Settings,
    /// Door frame with rightward arrow
    SignOut,
}

/// Sidebar toggle icon mode (hamburger / collapse icon).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidebarToggle {
    /// 'X' icon indicating collapse
    Collapse,
    /// Panel representation indicating expand
    Expand,
    /// Classical 3-bar hamburger icon
    Hamburger,
}

impl SidebarToggle {
    /// Selects Collapse when sidebar is visible, or Expand when hidden.
    #[must_use]
    pub fn from_visibility(sidebar_visible: bool) -> Self {
        if sidebar_visible {
            Self::Collapse
        } else {
            Self::Expand
        }
    }
}

/// Message actions share the same vector style as sidebar controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageIcon {
    NewChat,
    Read,
    Regenerate,
    Edit,
    Delete,
    Previous,
    Next,
}

/// Kind of vector icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconKind {
    Footer(FooterIcon),
    Sidebar(SidebarToggle),
    Message(MessageIcon),
}

impl From<FooterIcon> for IconKind {
    fn from(icon: FooterIcon) -> Self {
        Self::Footer(icon)
    }
}

impl From<SidebarToggle> for IconKind {
    fn from(icon: SidebarToggle) -> Self {
        Self::Sidebar(icon)
    }
}

impl From<MessageIcon> for IconKind {
    fn from(icon: MessageIcon) -> Self {
        Self::Message(icon)
    }
}

/// Configurable vector icon program for canvas rendering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VectorIcon {
    pub kind: IconKind,
    pub color: Option<Color>,
    pub stroke_width: f32,
}

impl VectorIcon {
    #[must_use]
    pub fn new(kind: impl Into<IconKind>) -> Self {
        Self {
            kind: kind.into(),
            color: None,
            stroke_width: 1.8,
        }
    }

    #[must_use]
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    #[must_use]
    pub fn with_stroke_width(mut self, stroke_width: f32) -> Self {
        self.stroke_width = stroke_width;
        self
    }
}

impl From<FooterIcon> for VectorIcon {
    fn from(icon: FooterIcon) -> Self {
        Self::new(icon)
    }
}

impl From<SidebarToggle> for VectorIcon {
    fn from(icon: SidebarToggle) -> Self {
        Self::new(icon)
    }
}

/// Geometric drawing routine for `FooterIcon::Characters` (two heads & shoulders).
pub fn draw_characters_icon(frame: &mut Frame, center: Point, stroke: Stroke) {
    frame.stroke(
        &Path::circle(center + Vector::new(-5.0, -5.0), 3.5),
        stroke,
    );
    frame.stroke(
        &Path::circle(center + Vector::new(5.0, -3.0), 3.0),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(-11.0, 7.0),
            center + Vector::new(1.0, 7.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(1.0, 7.0),
            center + Vector::new(10.0, 7.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(-11.0, 7.0),
            center + Vector::new(-8.0, 2.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(10.0, 7.0),
            center + Vector::new(7.0, 2.0),
        ),
        stroke,
    );
}

/// Geometric drawing routine for `FooterIcon::World` (globe with lat/long grid lines).
pub fn draw_world_icon(frame: &mut Frame, center: Point, stroke: Stroke) {
    frame.stroke(&Path::circle(center, 10.0), stroke);
    frame.stroke(
        &Path::line(
            center + Vector::new(-10.0, 0.0),
            center + Vector::new(10.0, 0.0),
        ),
        stroke,
    );
    let left_meridian = Path::new(|b| {
        b.move_to(center + Vector::new(0.0, -10.0));
        b.line_to(center + Vector::new(-3.5, -5.0));
        b.line_to(center + Vector::new(-4.5, 0.0));
        b.line_to(center + Vector::new(-3.5, 5.0));
        b.line_to(center + Vector::new(0.0, 10.0));
    });
    frame.stroke(&left_meridian, stroke);
    let right_meridian = Path::new(|b| {
        b.move_to(center + Vector::new(0.0, -10.0));
        b.line_to(center + Vector::new(3.5, -5.0));
        b.line_to(center + Vector::new(4.5, 0.0));
        b.line_to(center + Vector::new(3.5, 5.0));
        b.line_to(center + Vector::new(0.0, 10.0));
    });
    frame.stroke(&right_meridian, stroke);
}

/// Geometric drawing routine for `FooterIcon::Memory` (3 stacked database cylinders / ellipses).
pub fn draw_memory_icon(frame: &mut Frame, center: Point, stroke: Stroke) {
    for offset in [-6.0, 0.0, 6.0] {
        let ellipse = Path::new(|b| {
            b.ellipse(canvas::path::arc::Elliptical {
                center: center + Vector::new(0.0, offset),
                radii: Vector::new(9.0, 3.0),
                rotation: Radians(0.0),
                start_angle: Radians(0.0),
                end_angle: Radians(std::f32::consts::TAU),
            });
        });
        frame.stroke(&ellipse, stroke);
    }
    frame.stroke(
        &Path::line(
            center + Vector::new(-9.0, -6.0),
            center + Vector::new(-9.0, 6.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(9.0, -6.0),
            center + Vector::new(9.0, 6.0),
        ),
        stroke,
    );
}

/// Geometric drawing routine for `FooterIcon::Admin` (shield with checkmark).
pub fn draw_admin_icon(frame: &mut Frame, center: Point, stroke: Stroke) {
    let shield = Path::new(|b| {
        b.move_to(center + Vector::new(0.0, -10.0));
        b.line_to(center + Vector::new(8.0, -6.0));
        b.line_to(center + Vector::new(7.0, 3.0));
        b.line_to(center + Vector::new(0.0, 10.0));
        b.line_to(center + Vector::new(-7.0, 3.0));
        b.line_to(center + Vector::new(-8.0, -6.0));
        b.close();
    });
    frame.stroke(&shield, stroke);
    let checkmark = Path::new(|b| {
        b.move_to(center + Vector::new(-4.0, 0.0));
        b.line_to(center + Vector::new(-1.0, 3.0));
        b.line_to(center + Vector::new(5.0, -3.0));
    });
    frame.stroke(&checkmark, stroke);
}

/// Geometric drawing routine for `FooterIcon::Settings` (gear wheel with teeth/spokes).
pub fn draw_settings_icon(frame: &mut Frame, center: Point, stroke: Stroke) {
    frame.stroke(&Path::circle(center, 5.0), stroke);
    frame.stroke(&Path::circle(center, 1.5), stroke);
    let d = std::f32::consts::FRAC_1_SQRT_2;
    let directions = [
        Vector::new(0.0, -1.0),
        Vector::new(d, -d),
        Vector::new(1.0, 0.0),
        Vector::new(d, d),
        Vector::new(0.0, 1.0),
        Vector::new(-d, d),
        Vector::new(-1.0, 0.0),
        Vector::new(-d, -d),
    ];
    for direction in directions {
        frame.stroke(
            &Path::line(center + direction * 7.0, center + direction * 10.0),
            stroke,
        );
    }
}

/// Geometric drawing routine for `FooterIcon::SignOut` (door frame with arrow pointing right).
pub fn draw_sign_out_icon(frame: &mut Frame, center: Point, stroke: Stroke) {
    frame.stroke(
        &Path::line(
            center + Vector::new(-9.0, -9.0),
            center + Vector::new(-9.0, 9.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(-9.0, -9.0),
            center + Vector::new(-2.0, -9.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(-9.0, 9.0),
            center + Vector::new(-2.0, 9.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(-4.0, 0.0),
            center + Vector::new(9.0, 0.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(5.0, -4.0),
            center + Vector::new(9.0, 0.0),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(
            center + Vector::new(5.0, 4.0),
            center + Vector::new(9.0, 0.0),
        ),
        stroke,
    );
}

/// Geometric drawing routine for `FooterIcon` variants.
pub fn draw_footer_icon(frame: &mut Frame, icon: FooterIcon, center: Point, stroke: Stroke) {
    match icon {
        FooterIcon::Characters => draw_characters_icon(frame, center, stroke),
        FooterIcon::World => draw_world_icon(frame, center, stroke),
        FooterIcon::Memory => draw_memory_icon(frame, center, stroke),
        FooterIcon::Admin => draw_admin_icon(frame, center, stroke),
        FooterIcon::Settings => draw_settings_icon(frame, center, stroke),
        FooterIcon::SignOut => draw_sign_out_icon(frame, center, stroke),
    }
}

/// Geometric drawing routine for `SidebarToggle` variants.
pub fn draw_sidebar_toggle(
    frame: &mut Frame,
    toggle: SidebarToggle,
    center: Point,
    stroke: Stroke,
) {
    match toggle {
        SidebarToggle::Collapse => {
            frame.stroke(
                &Path::line(
                    center + Vector::new(-6.0, -6.0),
                    center + Vector::new(6.0, 6.0),
                ),
                stroke,
            );
            frame.stroke(
                &Path::line(
                    center + Vector::new(6.0, -6.0),
                    center + Vector::new(-6.0, 6.0),
                ),
                stroke,
            );
        }
        SidebarToggle::Expand => {
            let top_left = center + Vector::new(-11.0, -9.0);
            let panel = Path::rounded_rectangle(top_left, Size::new(22.0, 18.0), 3.0.into());
            frame.stroke(&panel, stroke);
            frame.stroke(
                &Path::line(
                    top_left + Vector::new(7.0, 0.0),
                    top_left + Vector::new(7.0, 18.0),
                ),
                stroke,
            );
        }
        SidebarToggle::Hamburger => {
            for y in [-6.0, 0.0, 6.0] {
                frame.stroke(
                    &Path::line(
                        center + Vector::new(-8.0, y),
                        center + Vector::new(8.0, y),
                    ),
                    stroke,
                );
            }
        }
    }
}

impl<Message> canvas::Program<Message, Theme, Renderer> for FooterIcon {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let scale = bounds.width.min(bounds.height) / 28.0;
        frame.scale(scale);
        let center = Point::new(14.0, 14.0);
        let stroke_color = theme.palette().text;
        let stroke = Stroke::default().with_color(stroke_color).with_width(1.8);
        draw_footer_icon(&mut frame, *self, center, stroke);
        vec![frame.into_geometry()]
    }
}

impl<Message> canvas::Program<Message, Theme, Renderer> for SidebarToggle {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = frame.center();
        let stroke_color = theme.palette().text;
        let stroke = Stroke::default().with_color(stroke_color).with_width(1.8);
        draw_sidebar_toggle(&mut frame, *self, center, stroke);
        vec![frame.into_geometry()]
    }
}

fn draw_message_icon(frame: &mut Frame, icon: MessageIcon, center: Point, stroke: Stroke) {
    let point = |x, y| center + Vector::new(x, y);
    let path = Path::new(|b| match icon {
        MessageIcon::NewChat => {
            b.move_to(point(-6.0, -8.0));
            b.line_to(point(6.0, -8.0));
            b.quadratic_curve_to(point(9.0, -8.0), point(9.0, -5.0));
            b.line_to(point(9.0, 3.0));
            b.quadratic_curve_to(point(9.0, 6.0), point(6.0, 6.0));
            b.line_to(point(-2.0, 6.0));
            b.line_to(point(-7.0, 10.0));
            b.line_to(point(-7.0, 6.0));
            b.quadratic_curve_to(point(-9.0, 6.0), point(-9.0, 3.0));
            b.line_to(point(-9.0, -5.0));
            b.quadratic_curve_to(point(-9.0, -8.0), point(-6.0, -8.0));
            b.close();
            b.move_to(point(-4.0, -1.0));
            b.line_to(point(4.0, -1.0));
            b.move_to(point(0.0, -5.0));
            b.line_to(point(0.0, 3.0));
        }
        MessageIcon::Edit => {
            b.move_to(point(-7.0, 4.0));
            b.line_to(point(3.0, -6.0));
            b.line_to(point(7.0, -2.0));
            b.line_to(point(-3.0, 8.0));
            b.line_to(point(-8.0, 9.0));
            b.close();
            b.move_to(point(1.0, -4.0));
            b.line_to(point(5.0, 0.0));
        }
        MessageIcon::Delete => {
            b.move_to(point(-8.0, -5.0));
            b.line_to(point(8.0, -5.0));
            b.move_to(point(-3.0, -5.0));
            b.line_to(point(-3.0, -8.0));
            b.line_to(point(3.0, -8.0));
            b.line_to(point(3.0, -5.0));
            b.move_to(point(-6.0, -5.0));
            b.line_to(point(-5.0, 8.0));
            b.line_to(point(5.0, 8.0));
            b.line_to(point(6.0, -5.0));
            for x in [-2.0, 2.0] {
                b.move_to(point(x, -1.0));
                b.line_to(point(x, 5.0));
            }
        }
        MessageIcon::Read => {
            b.move_to(point(-9.0, -3.0));
            b.line_to(point(-5.0, -3.0));
            b.line_to(point(0.0, -8.0));
            b.line_to(point(0.0, 8.0));
            b.line_to(point(-5.0, 3.0));
            b.line_to(point(-9.0, 3.0));
            b.close();
            b.move_to(point(3.0, -3.0));
            b.quadratic_curve_to(point(6.0, 0.0), point(3.0, 3.0));
            b.move_to(point(5.0, -6.0));
            b.quadratic_curve_to(point(11.0, 0.0), point(5.0, 6.0));
        }
        MessageIcon::Regenerate => {
            b.move_to(point(7.0, -3.0));
            b.bezier_curve_to(point(2.0, -12.0), point(-11.0, -6.0), point(-7.0, 5.0));
            b.bezier_curve_to(point(-3.0, 11.0), point(5.0, 9.0), point(8.0, 3.0));
            b.move_to(point(7.0, -9.0));
            b.line_to(point(7.0, -3.0));
            b.line_to(point(1.0, -3.0));
        }
        MessageIcon::Previous | MessageIcon::Next => {
            let direction = if icon == MessageIcon::Next { 1.0 } else { -1.0 };
            b.move_to(point(-3.0 * direction, -6.0));
            b.line_to(point(3.0 * direction, 0.0));
            b.line_to(point(-3.0 * direction, 6.0));
        }
    });
    frame.stroke(&path, stroke);
}

impl<Message> canvas::Program<Message, Theme, Renderer> for VectorIcon {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let center = frame.center();
        let stroke_color = self.color.unwrap_or(theme.palette().text);
        frame.translate(Vector::new(center.x, center.y));
        frame.scale((bounds.width.min(bounds.height) / 24.0).min(1.0));
        let center = Point::ORIGIN;
        let stroke = Stroke::default()
            .with_color(stroke_color)
            .with_width(self.stroke_width);
        match self.kind {
            IconKind::Footer(icon) => draw_footer_icon(&mut frame, icon, center, stroke),
            IconKind::Sidebar(toggle) => draw_sidebar_toggle(&mut frame, toggle, center, stroke),
            IconKind::Message(icon) => draw_message_icon(&mut frame, icon, center, stroke),
        }
        vec![frame.into_geometry()]
    }
}

// -----------------------------------------------------------------------------
// Icon Button Helpers
// -----------------------------------------------------------------------------

/// Creates a styled icon button with optional tooltip.
pub fn icon_button<'a, Message: Clone + 'a>(
    program: impl canvas::Program<Message, Theme, Renderer> + 'a,
    tooltip_text: Option<&'static str>,
    on_press: Message,
) -> Element<'a, Message> {
    sized_icon_button(program, tooltip_text, on_press, 44.0)
}

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

    let btn = button(container(icon_canvas).center_x(Length::Fill).center_y(Length::Fill))
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

/// Creates a standard 44x44 footer icon button with hover tooltip.
pub fn footer_icon_button<'a, Message: Clone + 'a>(
    icon: FooterIcon,
    tooltip_text: &'static str,
    on_press: Message,
) -> Element<'a, Message> {
    icon_button(icon, Some(tooltip_text), on_press)
}

/// Creates a styled ghost icon button with optional tooltip.
pub fn ghost_icon_button<'a, Message: Clone + 'a>(
    program: impl canvas::Program<Message, Theme, Renderer> + 'a,
    tooltip_text: Option<&'static str>,
    on_press: Message,
) -> Element<'a, Message> {
    let icon_canvas = canvas::Canvas::new(program)
        .width(24)
        .height(24);

    let btn = button(container(icon_canvas).center_x(Length::Fill).center_y(Length::Fill))
        .width(38)
        .height(38)
        .padding(0)
        .on_press(on_press)
        .style(|theme: &Theme, status: button::Status| {
            let palette = theme.palette();
            let (bg, border_color) = match status {
                button::Status::Active => (
                    Color::TRANSPARENT,
                    Color::TRANSPARENT,
                ),
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

// -----------------------------------------------------------------------------
// Markdown Rendering
// -----------------------------------------------------------------------------

/// Parsed markdown owned by the loaded conversation, never by a global cache.
#[derive(Debug, Clone, Default)]
pub struct Markdown {
    source: String,
    items: Vec<markdown::Item>,
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

// -----------------------------------------------------------------------------
// Toggler
// -----------------------------------------------------------------------------

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
                    Color::from_rgb(
                        100.0 / 255.0,
                        116.0 / 255.0,
                        139.0 / 255.0,
                    ),
                    Color::from_rgb(
                        241.0 / 255.0,
                        245.0 / 255.0,
                        249.0 / 255.0,
                    ),
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

// -----------------------------------------------------------------------------
// Button Styling Helpers
// -----------------------------------------------------------------------------

/// Style for primary action buttons (Send, Continue, Create, New Chat, etc.).
pub fn primary_button_style() -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, shadow_color) = match status {
            button::Status::Active => (
                COLOR_PRIMARY_STRONG,
                Color::from_rgba(0.0, 0.0, 0.0, 0.18),
            ),
            button::Status::Hovered => (
                COLOR_PRIMARY,
                Color::from_rgba(0.0, 0.0, 0.0, 0.28),
            ),
            button::Status::Pressed => (
                Color::from_rgb(0.25, 0.22, 0.75),
                Color::from_rgba(0.0, 0.0, 0.0, 0.10),
            ),
            button::Status::Disabled => (
                Color::from_rgba(0.4, 0.4, 0.5, 0.3),
                Color::TRANSPARENT,
            ),
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
pub fn secondary_button_style(light_mode: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, border_col, text_col) = match status {
            button::Status::Active => (
                if light_mode { Color::from_rgba(0.0, 0.0, 0.0, 0.06) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.08) },
                if light_mode { Color::from_rgba(0.0, 0.0, 0.0, 0.14) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.16) },
                if light_mode { Color::from_rgb(0.1, 0.15, 0.2) } else { Color::from_rgb(0.9, 0.9, 0.95) },
            ),
            button::Status::Hovered => (
                if light_mode { Color::from_rgba(0.0, 0.0, 0.0, 0.10) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.14) },
                if light_mode { Color::from_rgba(0.0, 0.0, 0.0, 0.22) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.26) },
                if light_mode { Color::from_rgb(0.05, 0.08, 0.12) } else { Color::WHITE },
            ),
            button::Status::Pressed => (
                if light_mode { Color::from_rgba(0.0, 0.0, 0.0, 0.15) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.20) },
                if light_mode { Color::from_rgba(0.0, 0.0, 0.3, 0.3) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.35) },
                if light_mode { Color::from_rgb(0.05, 0.08, 0.12) } else { Color::WHITE },
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
            button::Status::Disabled => (
                Color::from_rgba(0.8, 0.2, 0.2, 0.3),
                Color::TRANSPARENT,
            ),
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
pub fn ghost_button_style(light_mode: bool, on_accent: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let (bg, text_col) = if on_accent {
            match status {
                button::Status::Active => (
                    Color::TRANSPARENT,
                    Color::from_rgba(1.0, 1.0, 1.0, 0.85),
                ),
                button::Status::Hovered => (
                    Color::from_rgba(1.0, 1.0, 1.0, 0.22),
                    Color::WHITE,
                ),
                button::Status::Pressed => (
                    Color::from_rgba(1.0, 1.0, 1.0, 0.35),
                    Color::WHITE,
                ),
                button::Status::Disabled => (
                    Color::TRANSPARENT,
                    Color::from_rgba(1.0, 1.0, 1.0, 0.40),
                ),
            }
        } else {
            let text_col = if light_mode {
                Color::from_rgb(0.25, 0.30, 0.40)
            } else {
                Color::from_rgb(0.80, 0.84, 0.92)
            };
            match status {
                button::Status::Active => (
                    Color::TRANSPARENT,
                    text_col,
                ),
                button::Status::Hovered => (
                    if light_mode { Color::from_rgba(0.0, 0.0, 0.0, 0.08) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.12) },
                    if light_mode { Color::BLACK } else { Color::WHITE },
                ),
                button::Status::Pressed => (
                    if light_mode { Color::from_rgba(0.0, 0.0, 0.0, 0.15) } else { Color::from_rgba(1.0, 1.0, 1.0, 0.20) },
                    if light_mode { Color::BLACK } else { Color::WHITE },
                ),
                button::Status::Disabled => (
                    Color::TRANSPARENT,
                    Color::from_rgba(0.5, 0.5, 0.5, 0.4),
                ),
            }
        };

        button::Style {
            background: if bg == Color::TRANSPARENT { None } else { Some(Background::Color(bg)) },
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
pub fn chat_action_button_style(light_mode: bool, on_accent: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    ghost_button_style(light_mode, on_accent)
}

// -----------------------------------------------------------------------------
// Unit Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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
        for light in [true, false] {
            for desktop in [Color::BLACK, Color::WHITE] {
                let base = composite(desktop, surface_color(light, true));
                for background in [
                    raised_background(light, true),
                    sidebar_panel_style(light, true).background.unwrap(),
                ] {
                    let Background::Color(tint) = background else {
                        panic!("expected uniform glass tint");
                    };
                    let composed = composite(base, tint);
                    check(composed, palette(light).text);
                    check(composed, secondary_text_color(light));
                    check(composed, accent_text_color(light));
                }
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
    fn test_theme_palettes() {
        assert_eq!(LIGHT_PALETTE.primary, COLOR_PRIMARY);
        assert_eq!(DARK_PALETTE.primary, COLOR_PRIMARY);
        assert_eq!(palette(true).primary, COLOR_PRIMARY);
        assert_eq!(palette(false).primary, COLOR_PRIMARY);
        let _light = theme(true);
        let _dark = theme(false);
    }

    #[test]
    fn test_surface_color_math() {
        // The window ships with a clear base pane; text gets separate backing.
        let glass = surface_color(true, true);
        assert!((glass.a - glass_alpha(TRANSPARENCY)).abs() < 1e-6);
        assert!((glass.a - 0.52).abs() < 1e-4);

        // Transparency 0: the base is 66% opaque.
        assert!((glass_alpha(0.0) - 0.66).abs() < 1e-4);

        // Transparency 20: alpha 0.58.
        assert!((glass_alpha(20.0) - 0.58).abs() < 1e-4);

        // Transparency 80: alpha 0.34, with readable content plates above it.
        assert!((glass_alpha(80.0) - 0.34).abs() < 1e-4);

        // Transparency > 80 is clamped to 80
        assert_eq!(glass_alpha(100.0), glass_alpha(80.0));

        // Non-glass mode ignores the frost: alpha 1.0
        let non_glass = surface_color(true, false);
        assert_eq!(non_glass.a, 1.0);

        // Dark mode glass
        let dark_glass = surface_color(false, true);
        assert!((dark_glass.a - glass_alpha(TRANSPARENCY)).abs() < 1e-6);
        assert!(dark_glass.r < 0.1);

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
        let mixed = composite(Color::from_rgb(0.2, 0.2, 0.2), Color::from_rgba(1.0, 1.0, 1.0, 0.5));
        assert!((mixed.a - 1.0).abs() < 1e-6);
        assert!(mixed.r > 0.5);

        let tint = Color::from_rgba(0.1, 0.1, 0.1, 0.8);
        assert_eq!(glass_background(tint), Background::Color(tint));
    }

    #[test]
    fn test_surface_raised_and_border_colors() {
        let raised_light_glass = surface_raised_color(true, true);
        assert!((raised_light_glass.a - (235.0 / 255.0)).abs() < 1e-3);

        let raised_dark_glass = surface_raised_color(false, true);
        assert!((raised_dark_glass.a - (238.0 / 255.0)).abs() < 1e-3);

        // Raised plates stay readable over the frosted pane behind them
        assert!(raised_light_glass.a > surface_color(true, true).a);

        let border_light_glass = border_color(true, true);
        assert!((border_light_glass.a - (235.0 / 255.0)).abs() < 1e-3);

        let border_dark_glass = border_color(false, true);
        assert!((border_dark_glass.a - (87.0 / 255.0)).abs() < 1e-3);

        let border_opaque = border_color(true, false);
        assert_eq!(border_opaque.a, 1.0);
    }

    #[test]
    fn test_container_styles() {
        let card = card_style(true, false);
        assert!(card.background.is_some());
        assert_eq!(card.border.radius.top_left, 14.0);

        let modal = modal_dialog_frame_style(false, true);
        assert!(modal.background.is_some());
        assert_eq!(modal.border.radius.top_left, 20.0);

        // Popups must stay readable: the frame itself is nearly opaque glass
        let Some(Background::Color(tint)) = modal.background else {
            panic!("modal frame must use a uniform tint");
        };
        assert!(tint.a > 0.9);

        let scrim_light = modal_scrim_style(true);
        let scrim_dark = modal_scrim_style(false);
        assert!(scrim_light.background.is_some());
        assert!(scrim_dark.background.is_some());

        let user_bubble = user_chat_bubble_style();
        assert_eq!(user_bubble.text_color, Some(COLOR_ON_PRIMARY));
        assert_eq!(user_bubble.border.radius.top_left, 18.0);
        assert!(matches!(user_bubble.background, Some(Background::Color(_))));

        let assistant_bubble = assistant_chat_bubble_style(false, false);
        assert_eq!(assistant_bubble.border.radius.top_left, 18.0);

        let sidebar = sidebar_panel_style(false, true);
        assert_eq!(sidebar.border.radius.top_left, 18.0);
        assert!(matches!(sidebar.background, Some(Background::Color(_))));

        let window = window_backdrop_style(false, true);
        assert!(window.background.is_some());

        // Navigation has stronger backing than the clear window pane.
        let sidebar_dark = sidebar_panel_style(false, true);
        let Some(Background::Color(tint)) = sidebar_dark.background else {
            panic!("sidebar must use a uniform tint");
        };
        assert!(tint.a > surface_color(false, true).a);
    }

    #[test]
    fn test_tooltip_styles() {
        let light_tooltip = tooltip_style(&theme(true));
        assert!(light_tooltip.background.is_some());
        assert!(light_tooltip.shadow.blur_radius > 0.0);

        let dark_tooltip = tooltip_style(&theme(false));
        assert!(dark_tooltip.background.is_some());

        assert!(is_light_theme(&theme(true)));
        assert!(!is_light_theme(&theme(false)));
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

        let toggles = [
            SidebarToggle::Collapse,
            SidebarToggle::Expand,
            SidebarToggle::Hamburger,
        ];
        for toggle in toggles {
            let cloned = toggle;
            assert_eq!(toggle, cloned);
            let vec_toggle = VectorIcon::new(toggle);
            assert_eq!(vec_toggle.kind, IconKind::Sidebar(toggle));
        }

        assert_eq!(SidebarToggle::from_visibility(true), SidebarToggle::Collapse);
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
        let _footer_btn: Element<'_, ()> =
            footer_icon_button(FooterIcon::Characters, "Characters", ());
        let _toggle_btn: Element<'_, ()> = sidebar_toggle_button(true, ());
        let _ghost_btn: Element<'_, ()> = ghost_icon_button(SidebarToggle::Expand, Some("Expand"), ());
    }

    #[test]
    fn test_button_styles() {
        let dummy_theme = theme(false);

        let primary = primary_button_style()(&dummy_theme, button::Status::Active);
        assert!(primary.background.is_some());

        let secondary = secondary_button_style(false)(&dummy_theme, button::Status::Active);
        assert!(secondary.background.is_some());

        let danger = danger_button_style()(&dummy_theme, button::Status::Active);
        assert!(danger.background.is_some());

        let chat_action = chat_action_button_style(false, false)(&dummy_theme, button::Status::Active);
        assert!(chat_action.background.is_none());
        let chat_action_hovered = chat_action_button_style(false, false)(&dummy_theme, button::Status::Hovered);
        assert!(chat_action_hovered.background.is_some());

        let chat_action_accent = chat_action_button_style(false, true)(&dummy_theme, button::Status::Active);
        assert!(chat_action_accent.background.is_none());
        let chat_action_accent_hovered = chat_action_button_style(false, true)(&dummy_theme, button::Status::Hovered);
        assert!(chat_action_accent_hovered.background.is_some());
    }
}
