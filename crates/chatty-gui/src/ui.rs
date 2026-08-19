//! UI module for rendering chat components and dialogs

use super::*;

impl ChattyApp {
    pub(super) fn popup_max_height(ctx: &egui::Context) -> f32 {
        ctx.content_rect().height() * 0.75
    }

    /// Render a markdown line with formatting
    fn markdown_line(&mut self, ui: &mut egui::Ui, text: &str, size: f32) {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = ui.available_width();
        let base = egui::TextFormat {
            font_id: egui::FontId::new(size, egui::FontFamily::Proportional),
            color: ui.visuals().text_color(),
            ..Default::default()
        };
        let mut remaining = text;
        let mut first_link = None;
        while !remaining.is_empty() {
            let (marker, closing, mut format) = if remaining.starts_with("**") {
                ("**", "**", base.clone())
            } else if remaining.starts_with("~~") {
                let mut format = base.clone();
                format.strikethrough = egui::Stroke::new(1.0, format.color);
                ("~~", "~~", format)
            } else if remaining.starts_with('`') {
                let mut format = base.clone();
                format.font_id = egui::FontId::new(size, egui::FontFamily::Monospace);
                format.background = ui.visuals().faint_bg_color;
                ("`", "`", format)
            } else if remaining.starts_with('*') || remaining.starts_with('_') {
                let marker = &remaining[..1];
                let mut format = base.clone();
                format.italics = true;
                (marker, marker, format)
            } else {
                ("", "", base.clone())
            };

            if !marker.is_empty()
                && let Some(end) = remaining[marker.len()..].find(closing)
            {
                let content_end = marker.len() + end;
                if marker == "**" {
                    format.color = ui.visuals().strong_text_color();
                    format.extra_letter_spacing = 0.2;
                }
                job.append(&remaining[marker.len()..content_end], 0.0, format);
                remaining = &remaining[content_end + closing.len()..];
                continue;
            }

            if remaining.starts_with('[')
                && let Some(label_end) = remaining.find("](")
                && let Some(url_end_offset) = remaining[label_end + 2..].find(')')
            {
                let url_end = label_end + 2 + url_end_offset;
                let url = &remaining[label_end + 2..url_end];
                if url.starts_with("https://") || url.starts_with("http://") {
                    let mut format = base.clone();
                    format.color = ui.visuals().hyperlink_color;
                    format.underline = egui::Stroke::new(1.0, format.color);
                    job.append(&remaining[1..label_end], 0.0, format);
                    first_link.get_or_insert_with(|| url.to_owned());
                    remaining = &remaining[url_end + 1..];
                    continue;
                }
            }

            let next = remaining
                .char_indices()
                .skip(1)
                .find(|(_, character)| matches!(character, '*' | '_' | '`' | '[' | '~'))
                .map(|(index, _)| index)
                .unwrap_or(remaining.len());
            job.append(&remaining[..next], 0.0, base.clone());
            remaining = &remaining[next..];
        }
        let response = ui.add(egui::Label::new(job).wrap().sense(if first_link.is_some() {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        }));
        if let Some(url) = first_link {
            let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            }
        }
    }

    /// Render formatted markdown text
    pub(crate) fn render_markdown(&mut self, ui: &mut egui::Ui, markdown: &str) {
        let mut code = None::<String>;
        for line in markdown.lines() {
            if line.trim_start().starts_with("```") {
                if let Some(code) = code.take() {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .corner_radius(6.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(egui::RichText::new(code).monospace()).wrap());
                        });
                } else {
                    code = Some(String::new());
                }
                continue;
            }
            if let Some(code) = &mut code {
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(line);
                continue;
            }
            if line.trim().is_empty() {
                ui.add_space(4.0);
                continue;
            }
            if line.trim() == "---" || line.trim() == "***" {
                ui.separator();
                continue;
            }
            let trimmed = line.trim_start();
            let heading_level = trimmed
                .chars()
                .take_while(|character| *character == '#')
                .count();
            if (1..=6).contains(&heading_level)
                && trimmed.as_bytes().get(heading_level) == Some(&b' ')
            {
                self.markdown_line(
                    ui,
                    trimmed[heading_level + 1..].trim(),
                    (25.0 - heading_level as f32 * 1.8).max(16.0),
                );
            } else if let Some(item) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .or_else(|| trimmed.strip_prefix("+ "))
            {
                ui.horizontal_wrapped(|ui| {
                    ui.label("•");
                    self.markdown_line(ui, item, 16.0);
                });
            } else if let Some(item) = trimmed.strip_prefix("> ") {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| self.markdown_line(ui, item, 16.0));
            } else if let Some((number, item)) = trimmed.split_once(". ")
                && number.chars().all(|character| character.is_ascii_digit())
            {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{number}."));
                    self.markdown_line(ui, item, 16.0);
                });
            } else {
                self.markdown_line(ui, line, 16.0);
            }
        }
        if let Some(code) = code {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(6.0)
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(code).monospace()).wrap());
                });
        }
    }

    /// Toggle switch widget
    pub(super) fn toggle_switch(
        ui: &mut egui::Ui,
        value: &mut bool,
        label: &str,
    ) -> egui::Response {
        let desired_size = egui::vec2(34.0, 20.0);
        let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
        if response.clicked() {
            *value = !*value;
            response.mark_changed();
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, label)
        });

        if ui.is_rect_visible(rect) {
            let animation = ui.ctx().animate_bool(response.id, *value);
            let visuals = ui.style().interact_selectable(&response, *value);
            let radius = rect.height() / 2.0;
            let fill = if *value {
                ui.visuals().selection.bg_fill
            } else {
                visuals.bg_fill
            };
            ui.painter().rect_filled(rect, radius, fill);
            let knob_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), animation);
            ui.painter().circle_filled(
                egui::pos2(knob_x, rect.center().y),
                radius - 3.0,
                visuals.fg_stroke.color,
            );
        }

        response
    }
}
