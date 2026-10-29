//! Vector canvas icons.

use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::{Color, Point, Radians, Rectangle, Renderer, Size, Theme, Vector};

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
    Copy,
    Previous,
    Next,
    Add,
    Save,
    Import,
    Export,
    Sparkle,
    Emotions,
    Image,
    Folder,
    Star,
    Pin,
    Merge,
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
    frame.stroke(&Path::circle(center + Vector::new(-5.0, -5.0), 3.5), stroke);
    frame.stroke(&Path::circle(center + Vector::new(5.0, -3.0), 3.0), stroke);
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
        MessageIcon::Copy => {
            b.rectangle(point(-4.0, -4.0), Size::new(11.0, 12.0));
            b.move_to(point(-7.0, 4.0));
            b.line_to(point(-7.0, -8.0));
            b.line_to(point(3.0, -8.0));
        }
        MessageIcon::Add => {
            b.move_to(point(-7.0, 0.0));
            b.line_to(point(7.0, 0.0));
            b.move_to(point(0.0, -7.0));
            b.line_to(point(0.0, 7.0));
        }
        MessageIcon::Save => {
            b.move_to(point(-8.0, -8.0));
            b.line_to(point(5.0, -8.0));
            b.line_to(point(8.0, -5.0));
            b.line_to(point(8.0, 8.0));
            b.line_to(point(-8.0, 8.0));
            b.close();
            b.move_to(point(-4.0, -8.0));
            b.line_to(point(-4.0, -3.0));
            b.line_to(point(3.0, -3.0));
            b.line_to(point(3.0, -8.0));
            b.rectangle(point(-5.0, 2.0), Size::new(10.0, 6.0));
        }
        MessageIcon::Import | MessageIcon::Export => {
            b.move_to(point(-8.0, 3.0));
            b.line_to(point(-8.0, 8.0));
            b.line_to(point(8.0, 8.0));
            b.line_to(point(8.0, 3.0));
            let (from, to) = if icon == MessageIcon::Import {
                (-9.0, 3.0)
            } else {
                (4.0, -8.0)
            };
            let head = if icon == MessageIcon::Import {
                -4.0
            } else {
                4.0
            };
            b.move_to(point(0.0, from));
            b.line_to(point(0.0, to));
            b.move_to(point(-4.0, to + head));
            b.line_to(point(0.0, to));
            b.line_to(point(4.0, to + head));
        }
        MessageIcon::Sparkle => {
            b.move_to(point(-2.0, -7.0));
            b.quadratic_curve_to(point(-1.0, 0.0), point(6.0, 1.0));
            b.quadratic_curve_to(point(-1.0, 2.0), point(-2.0, 9.0));
            b.quadratic_curve_to(point(-3.0, 2.0), point(-10.0, 1.0));
            b.quadratic_curve_to(point(-3.0, 0.0), point(-2.0, -7.0));
            b.move_to(point(7.0, -9.0));
            b.line_to(point(7.0, -4.0));
            b.move_to(point(4.5, -6.5));
            b.line_to(point(9.5, -6.5));
        }
        MessageIcon::Emotions => {
            b.circle(point(0.0, 0.0), 8.5);
            b.move_to(point(-3.0, -3.5));
            b.line_to(point(-3.0, -1.5));
            b.move_to(point(3.0, -3.5));
            b.line_to(point(3.0, -1.5));
            b.move_to(point(-4.5, 2.0));
            b.quadratic_curve_to(point(0.0, 7.0), point(4.5, 2.0));
        }
        MessageIcon::Image => {
            b.rectangle(point(-9.0, -7.0), Size::new(18.0, 14.0));
            b.move_to(point(-9.0, 5.0));
            b.line_to(point(-3.0, -1.0));
            b.line_to(point(1.0, 3.0));
            b.line_to(point(4.0, 0.0));
            b.line_to(point(9.0, 5.0));
            b.circle(point(4.0, -3.0), 1.5);
        }
        MessageIcon::Folder => {
            b.move_to(point(-9.0, -6.0));
            b.line_to(point(-3.0, -6.0));
            b.line_to(point(-1.0, -4.0));
            b.line_to(point(9.0, -4.0));
            b.line_to(point(9.0, 7.0));
            b.line_to(point(-9.0, 7.0));
            b.close();
        }
        MessageIcon::Star => {
            for i in 0..10 {
                let radius = if i % 2 == 0 { 9.0 } else { 4.0 };
                let angle = std::f32::consts::PI * (i as f32 / 5.0 - 0.5);
                let p = point(radius * angle.cos(), radius * angle.sin() + 0.5);
                if i == 0 {
                    b.move_to(p);
                } else {
                    b.line_to(p);
                }
            }
            b.close();
        }
        MessageIcon::Pin => {
            b.move_to(point(-4.0, -8.0));
            b.line_to(point(4.0, -8.0));
            b.move_to(point(-2.0, -8.0));
            b.line_to(point(-3.0, -1.0));
            b.move_to(point(2.0, -8.0));
            b.line_to(point(3.0, -1.0));
            b.move_to(point(-7.0, -1.0));
            b.line_to(point(7.0, -1.0));
            b.move_to(point(0.0, -1.0));
            b.line_to(point(0.0, 9.0));
        }
        MessageIcon::Merge => {
            b.move_to(point(-8.0, -7.0));
            b.line_to(point(-1.0, 0.0));
            b.line_to(point(-8.0, 7.0));
            b.move_to(point(-1.0, 0.0));
            b.line_to(point(8.0, 0.0));
            b.move_to(point(4.0, -4.0));
            b.line_to(point(8.0, 0.0));
            b.line_to(point(4.0, 4.0));
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
