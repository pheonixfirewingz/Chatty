//! Context indicator for the most recent broker generation, scoped to a conversation.

use chatty_protocol::GenerationContext;
use iced::widget::{canvas, container, row, text, tooltip};
use iced::{Alignment, Element, Radians, Rectangle, Renderer, Theme, mouse};

fn fraction(context: &GenerationContext) -> Option<f64> {
    (context.context_tokens > 0).then(|| {
        context
            .prompt_tokens
            .saturating_add(context.completion_tokens) as f64
            / f64::from(context.context_tokens)
    })
}

fn description(context: Option<&GenerationContext>) -> String {
    let Some(context) = context.filter(|context| context.context_tokens > 0) else {
        return "Context usage is available after you send a message.".into();
    };
    let total = context
        .prompt_tokens
        .saturating_add(context.completion_tokens);
    format!(
        "Context used: {}{} / {} tokens ({:.1}%)",
        if context.estimated { "~" } else { "" },
        crate::admin_monitor::format_token_count(total),
        crate::admin_monitor::format_token_count(u64::from(context.context_tokens)),
        fraction(context).unwrap_or(0.0) * 100.0,
    )
}

pub fn indicator<'a, Message: 'a>(context: Option<&GenerationContext>) -> Element<'a, Message> {
    let fraction = context.and_then(fraction);
    let label = fraction.map_or_else(|| "—".into(), |used| format!("{:.0}%", used * 100.0));
    tooltip(
        row![
            canvas::Canvas::new(ContextPie { fraction })
                .width(24)
                .height(24),
            text(label).size(11),
        ]
        .spacing(5)
        .align_y(Alignment::Center),
        container(text(description(context)).size(12))
            .padding(10)
            .style(crate::ui::tooltip_style),
        tooltip::Position::Top,
    )
    .gap(8)
    .into()
}

struct ContextPie {
    fraction: Option<f64>,
}

impl<Message> canvas::Program<Message> for ContextPie {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = frame.center();
        let radius = bounds.width.min(bounds.height) / 2.0 - 2.0;
        let circle = canvas::Path::circle(center, radius);
        frame.fill(&circle, theme.extended_palette().background.strong.color);
        if let Some(fraction) = self.fraction {
            let color = if fraction >= 0.9 {
                theme.extended_palette().danger.base.color
            } else {
                theme.extended_palette().primary.base.color
            };
            if fraction >= 1.0 {
                frame.fill(&circle, color);
            } else if fraction > 0.0 {
                let slice = canvas::Path::new(|path| {
                    path.move_to(center);
                    path.line_to(iced::Point::new(center.x, center.y - radius));
                    path.arc(canvas::path::Arc {
                        center,
                        radius,
                        start_angle: Radians(-std::f32::consts::FRAC_PI_2),
                        end_angle: Radians(
                            -std::f32::consts::FRAC_PI_2 + fraction as f32 * std::f32::consts::TAU,
                        ),
                    });
                    path.close();
                });
                frame.fill(&slice, color);
            }
        }
        frame.stroke(
            &circle,
            canvas::Stroke::default()
                .with_color(theme.palette().text.scale_alpha(0.6))
                .with_width(1.0),
        );
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
