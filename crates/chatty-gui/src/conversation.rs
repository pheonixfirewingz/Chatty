use super::*;

impl ChattyApp {
    pub(super) fn displayed_message_content(message: &ChatMessage) -> &str {
        message
            .selected_variant_id
            .as_deref()
            .and_then(|selected| {
                message
                    .variants
                    .iter()
                    .find(|variant| variant.id == selected)
            })
            .map(|variant| variant.content.as_str())
            .unwrap_or(&message.content)
    }

    fn character_name(&self, id: Option<&str>) -> String {
        id.and_then(|id| self.characters.iter().find(|c| c.id == id))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Assistant".into())
    }

    fn avatar(ui: &mut egui::Ui, name: &str, size: f32) {
        Self::character_portrait(ui, name, None, name, size);
    }

    pub(super) fn character_portrait(
        ui: &mut egui::Ui,
        image_id: &str,
        data: Option<&[u8]>,
        name: &str,
        size: f32,
    ) {
        Self::character_image(ui, image_id, data, name, size, size / 2.0);
    }

    fn character_image(
        ui: &mut egui::Ui,
        image_id: &str,
        data: Option<&[u8]>,
        name: &str,
        size: f32,
        corner_radius: f32,
    ) {
        let texture = data.and_then(|data| Self::cache_character_portrait(ui.ctx(), image_id, data));
        if let Some(texture) = texture {
            let uv = Self::top_center_square_uv(texture.size_vec2());
            ui.add(
                egui::Image::new(&texture)
                    .fit_to_exact_size(egui::vec2(size, size))
                    .uv(uv)
                    .corner_radius(corner_radius),
            );
            return;
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), size / 2.0, COLOR_PRIMARY);
        let initial = name.chars().next().unwrap_or('A').to_ascii_uppercase();
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            initial,
            egui::FontId::new(size * 0.42, egui::FontFamily::Proportional),
            egui::Color32::WHITE,
        );
    }

    pub(super) fn top_center_square_uv(source: egui::Vec2) -> egui::Rect {
        if source.x > source.y {
            let margin = (1.0 - source.y / source.x) / 2.0;
            egui::Rect::from_min_max(egui::pos2(margin, 0.0), egui::pos2(1.0 - margin, 1.0))
        } else {
            // Keep the top of portrait images and trim the lower edge. This preserves
            // faces much more reliably than a vertically centered square crop.
            egui::Rect::from_min_max(
                egui::pos2(0.0, 0.0),
                egui::pos2(1.0, source.x / source.y),
            )
        }
    }

    pub(super) fn current_emotion_portrait(&self) -> Option<(String, String, String, Vec<u8>)> {
        self.selected_conversation.as_ref()?;
        for message in self.messages.iter().rev() {
            let (Some(character_id), Some(image_id)) =
                (message.author_id.as_deref(), message.character_image_id.as_deref())
            else {
                continue;
            };
            let Some(character) = self
                .characters
                .iter()
                .find(|character| character.id == character_id)
            else {
                continue;
            };
            if let Some(image) = character.images.iter().find(|image| image.id == image_id) {
                return Some((
                    character.name.clone(),
                    image.label.clone(),
                    image.id.clone(),
                    image.data.clone(),
                ));
            }
        }
        let character_id = self.selected_speaker()?;
        let character = self
            .characters
            .iter()
            .find(|character| character.id == character_id)?;
        let image_id = character.default_image_id.as_deref()?;
        let image = character.images.iter().find(|image| image.id == image_id)?;
        Some((
            character.name.clone(),
            image.label.clone(),
            image.id.clone(),
            image.data.clone(),
        ))
    }

    fn render_emotion_window(
        ui: &mut egui::Ui,
        portrait: &(String, String, String, Vec<u8>),
        image_size: f32,
    ) {
        egui::Frame::new()
            .fill(color_surface_raised(ui))
            .stroke(egui::Stroke::new(1.0, color_border(ui)))
            .corner_radius(14.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    Self::character_image(
                        ui,
                        &portrait.2,
                        Some(&portrait.3),
                        &format!("{} — {}", portrait.0, portrait.1),
                        image_size,
                        image_size / 2.0,
                    );
                    ui.add_space(3.0);
                    ui.label(egui::RichText::new(&portrait.0).strong());
                    ui.label(
                        egui::RichText::new(format!("Emotion: {}", portrait.1)).size(13.0),
                    );
                });
            });
    }

    pub(super) fn cache_character_portraits(&mut self, ctx: &egui::Context) {
        for (image_id, data) in self.received_character_images.drain(..) {
            let _ = Self::cache_character_portrait(ctx, &image_id, &data);
        }

        let mut requests = Vec::new();
        for character in &self.characters {
            for image in &character.images {
                if !image.data.is_empty() {
                    let _ = Self::cache_character_portrait(ctx, &image.id, &image.data);
                    continue;
                }
                let texture_key = egui::Id::new(("character-portrait", &image.id));
                let cached = ctx.data_mut(|store| {
                    store
                        .get_temp::<egui::TextureHandle>(texture_key)
                        .is_some()
                });
                let request_key = (character.id.clone(), image.id.clone());
                if !cached && !self.character_image_requests.contains(&request_key) {
                    requests.push(request_key);
                }
            }
        }
        for (character_id, image_id) in requests {
            self.character_image_requests
                .insert((character_id.clone(), image_id.clone()));
            self.send(Request::GetCharacterImage {
                session_token: self.token.clone(),
                character_id,
                image_id,
            });
        }
    }

    fn cache_character_portrait(
        ctx: &egui::Context,
        image_id: &str,
        data: &[u8],
    ) -> Option<egui::TextureHandle> {
        let texture_key = egui::Id::new(("character-portrait", image_id));
        if let Some(texture) =
            ctx.data_mut(|store| store.get_temp::<egui::TextureHandle>(texture_key))
        {
            return Some(texture);
        }
        let dimensions = image::ImageReader::with_format(
            std::io::Cursor::new(data),
            image::ImageFormat::Png,
        )
        .into_dimensions()
        .ok()?;
        if dimensions.0 < 128
            || dimensions.1 < 128
            || dimensions.0 > 512
            || dimensions.1 > 512
        {
            return None;
        }
        let rgba = image::load_from_memory_with_format(data, image::ImageFormat::Png)
            .ok()?
            .to_rgba8();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [rgba.width() as usize, rgba.height() as usize],
            rgba.as_raw(),
        );
        let texture = ctx.load_texture(
            format!("character-portrait-{image_id}"),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        ctx.data_mut(|store| store.insert_temp(texture_key, texture.clone()));
        Some(texture)
    }

    pub(super) fn render_chat(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_rect_before_wrap();
        let compact = available.width() < 760.0;
        let emotion_portrait = self.current_emotion_portrait();
        let sidebar_toggle_height = if self.sidebar_visible { 0.0 } else { 50.0 };
        let typing_height = if self.typing_character.is_some() {
            24.0
        } else {
            0.0
        };
        let composer_height = if compact { 94.0 } else { 104.0 };
        let content_width = available.width() - if compact { 0.0 } else { 28.0 };
        let content_left = available.center().x - content_width / 2.0;
        let mut messages_rect = egui::Rect::from_min_max(
            egui::pos2(content_left, available.min.y + sidebar_toggle_height),
            egui::pos2(
                content_left + content_width,
                (available.max.y - composer_height - typing_height).max(available.min.y),
            ),
        );
        let emotion_rect = emotion_portrait.as_ref().map(|_| {
            if compact {
                let size = egui::vec2(128.0, 160.0);
                messages_rect.min.y =
                    (messages_rect.min.y + size.y + 12.0).min(messages_rect.max.y);
                egui::Rect::from_min_size(
                    egui::pos2(
                        content_left + content_width - size.x - 8.0,
                        available.min.y + sidebar_toggle_height + 8.0,
                    ),
                    size,
                )
            } else {
                let size = egui::vec2(216.0, 250.0);
                messages_rect.max.x =
                    (messages_rect.max.x - size.x - 18.0).max(messages_rect.min.x);
                egui::Rect::from_min_size(
                    egui::pos2(
                        content_left + content_width - size.x,
                        available.min.y + sidebar_toggle_height + 8.0,
                    ),
                    size,
                )
            }
        });
        let composer_rect = egui::Rect::from_min_max(
            egui::pos2(content_left, available.max.y - composer_height),
            egui::pos2(
                content_left + content_width - if compact { 13.0 } else { 0.0 },
                available.max.y,
            ),
        );

        if !self.sidebar_visible {
            let toggle_rect = egui::Rect::from_min_size(
                egui::pos2(content_left, available.min.y),
                egui::vec2(44.0, 44.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(toggle_rect), |ui| {
                if Self::sidebar_toggle_button(ui, false).clicked() {
                    self.sidebar_visible = true;
                }
            });
        }

        if let (Some(rect), Some(portrait)) = (emotion_rect, emotion_portrait.as_ref()) {
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                Self::render_emotion_window(
                    ui,
                    portrait,
                    if compact { 96.0 } else { 180.0 },
                );
            });
        }

        ui.scope_builder(egui::UiBuilder::new().max_rect(composer_rect), |ui| {
            ui.add_space(8.0);
            egui::Frame::new()
                .fill(color_surface_raised(ui))
                .stroke(egui::Stroke::new(1.0, color_border(ui)))
                .corner_radius(16.0)
                .inner_margin(egui::Margin::symmetric(12, 9))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let response = ui.add_sized(
                            [(ui.available_width() - 86.0).max(120.0), 42.0],
                            egui::TextEdit::multiline(&mut self.input)
                                .hint_text("Message your character...")
                                .frame(egui::Frame::NONE)
                                .vertical_align(egui::Align::Center)
                                .desired_rows(1),
                        );
                        let enter = response.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                        if let Some(id) = self.active_request {
                            if ui
                                .add_sized([70.0, 40.0], egui::Button::new("Stop"))
                                .clicked()
                            {
                                let _ = self.commands.send(Command::Cancel(id));
                            }
                        } else {
                            let label = if self.input.trim().is_empty() {
                                "Continue"
                            } else {
                                "Send"
                            };
                            if ui
                                .add_sized(
                                    [70.0, 40.0],
                                    egui::Button::new(egui::RichText::new(label).strong())
                                        .fill(COLOR_PRIMARY_STRONG)
                                        .corner_radius(11.0),
                                )
                                .clicked()
                                || enter
                            {
                                self.submit_message();
                            }
                        }
                    });
                    if !compact {
                        ui.label(
                            egui::RichText::new("Enter to send  ·  Shift + Enter for a new line")
                                .size(11.0)
                                .weak(),
                        );
                    }
                });
        });
        if let Some(id) = self.typing_character.as_deref() {
            let typing_rect = egui::Rect::from_min_max(
                egui::pos2(content_left + 8.0, messages_rect.max.y),
                egui::pos2(content_left + content_width, composer_rect.min.y),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(typing_rect), |ui| {
                let status = match self.generation_status {
                    Some(GenerationStatus::SelectingEmotion) => {
                        "AI is selecting an emotion portrait…".to_owned()
                    }
                    None => format!("{} is typing…", self.character_name(Some(id))),
                };
                ui.label(
                    egui::RichText::new(status).italics().weak(),
                );
            });
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(messages_rect), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("chat-messages")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add_space(24.0);
                    if self.selected_conversation.is_none() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.32);
                            ui.heading(egui::RichText::new("Who will you meet today?").size(24.0));
                            ui.label(
                                egui::RichText::new(
                                    "Pick a character, set the scene, and start a conversation.",
                                )
                                .weak(),
                            );
                            ui.add_space(16.0);
                            if ui
                                .add_sized(
                                    [168.0, 42.0],
                                    egui::Button::new(
                                        egui::RichText::new("Start a new chat").strong(),
                                    )
                                    .fill(COLOR_PRIMARY_STRONG)
                                    .corner_radius(11.0),
                                )
                                .clicked()
                            {
                                self.new_chat_open = true;
                            }
                        });
                        return;
                    }
                    for message in self.messages.clone() {
                        ui.push_id(&message.id, |ui| self.render_message(ui, &message));
                        ui.add_space(18.0);
                    }
                    if !self.stream_text.is_empty() {
                        let name = self.character_name(self.typing_character.as_deref());
                        let text = self.stream_text.clone();
                        ui.push_id("streaming-message", |ui| {
                            ui.vertical(|ui| {
                                ui.strong(egui::RichText::new(format!("{name}:")).size(16.0));
                                self.render_markdown(ui, &text);
                            });
                        });
                    }
                });
        });
    }
    fn render_message(&mut self, ui: &mut egui::Ui, message: &ChatMessage) {
        let user = message.author_type == "user";
        if self.editing_message_id.as_deref() == Some(message.id.as_str()) {
            self.render_message_editor(ui, message, user);
            return;
        }
        let rendered = ui.scope(|ui| {
            if user {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    egui::Frame::new()
                        .fill(COLOR_PRIMARY_STRONG)
                        .corner_radius(16.0)
                        .inner_margin(egui::Margin::symmetric(15, 7))
                        .show(ui, |ui| {
                            ui.set_max_width((ui.available_width() * 0.72).clamp(150.0, 620.0));
                            ui.visuals_mut().override_text_color = Some(COLOR_ON_PRIMARY);
                            ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                self.render_markdown(ui, Self::displayed_message_content(message))
                            });
                        });
                });
            } else {
                let name = self.character_name(message.author_id.as_deref());
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{name}:"))
                            .size(14.0)
                            .strong()
                            .color(color_primary_text(ui)),
                    );
                    ui.add_space(1.0);
                    self.render_markdown(ui, Self::displayed_message_content(message));
                });
            }
        });
        let message_rect = rendered.response.rect;
        let hovered = ui
            .input(|input| input.pointer.hover_pos())
            .is_some_and(|pointer| message_rect.contains(pointer));
        let delete_rect = egui::Rect::from_min_size(
            egui::pos2(message_rect.right() - 20.0, message_rect.top()),
            egui::vec2(18.0, 24.0),
        );
        if hovered {
            let edit_rect = egui::Rect::from_min_size(
                egui::pos2(message_rect.right() - 55.0, message_rect.top()),
                egui::vec2(34.0, 24.0),
            );
            if ui
                .put(
                    edit_rect,
                    egui::Button::new(egui::RichText::new("Edit").size(10.0)).frame(false),
                )
                .on_hover_text("Edit message")
                .clicked()
            {
                self.editing_message_id = Some(message.id.clone());
                self.editing_message_content = message.content.clone();
            }
            if !user {
                let regenerate_rect = delete_rect.translate(egui::vec2(-54.0, 1.0));
                if ui
                    .put(
                        regenerate_rect,
                        egui::Button::new(egui::RichText::new("↻").size(8.0)).frame(false),
                    )
                    .on_hover_text("Regenerate response")
                    .clicked()
                {
                    self.regenerate(message);
                }
            }
            if ui
                .put(delete_rect, egui::Button::new("×").frame(false))
                .on_hover_text("Delete")
                .clicked()
            {
                self.delete_message(&message.id);
            }
        }
    }
    fn render_message_editor(&mut self, ui: &mut egui::Ui, message: &ChatMessage, user: bool) {
        let frame = egui::Frame::new()
            .fill(if user {
                COLOR_PRIMARY_STRONG
            } else {
                color_surface_raised(ui)
            })
            .stroke(egui::Stroke::new(1.0, color_border(ui)))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(12, 9));
        if user {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                frame.show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                        self.render_message_editor_contents(ui, message)
                    });
                });
            });
        } else {
            frame.show(ui, |ui| self.render_message_editor_contents(ui, message));
        }
    }
    fn render_message_editor_contents(&mut self, ui: &mut egui::Ui, message: &ChatMessage) {
        ui.set_width((ui.available_width() * 0.72).clamp(240.0, 620.0));
        let response = ui.add_sized(
            [ui.available_width(), 96.0],
            egui::TextEdit::multiline(&mut self.editing_message_content)
                .desired_rows(4)
                .hint_text("Message text (Markdown supported)"),
        );
        if response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.cancel_message_edit();
            return;
        }
        ui.horizontal(|ui| {
            let valid = !self.editing_message_content.trim().is_empty()
                && self.editing_message_content != message.content;
            if ui.add_enabled(valid, egui::Button::new("Save")).clicked() {
                let content = self.editing_message_content.clone();
                self.send(Request::UpdateMessage {
                    session_token: self.token.clone(),
                    message_id: message.id.clone(),
                    content,
                });
                self.cancel_message_edit();
            }
            if ui.button("Cancel").clicked() {
                self.cancel_message_edit();
            }
            ui.label(egui::RichText::new("Esc to cancel").size(11.0).weak());
        });
    }
    fn cancel_message_edit(&mut self) {
        self.editing_message_id = None;
        self.editing_message_content.clear();
    }
    fn delete_message(&self, id: &str) {
        self.send(Request::DeleteEntity {
            session_token: self.token.clone(),
            kind: EntityKind::Message,
            entity_id: id.into(),
        });
    }
    pub(super) fn regenerate(&self, message: &ChatMessage) {
        if let Some(cid) = self.selected_conversation.clone() {
            self.send(Request::Generate {
                session_token: self.token.clone(),
                conversation_id: cid,
                speaker_id: message.author_id.clone(),
                parent_id: Some(message.id.clone()),
            });
        }
    }
    pub(super) fn submit_message(&mut self) {
        let Some(cid) = self.selected_conversation.clone() else {
            self.new_chat_open = true;
            return;
        };
        let speaker = self.selected_speaker();
        if self.input.trim().is_empty() {
            self.send(Request::Generate {
                session_token: self.token.clone(),
                conversation_id: cid,
                speaker_id: speaker,
                parent_id: None,
            });
            return;
        }
        let content = std::mem::take(&mut self.input);
        let message = Request::SendMessage {
            session_token: self.token.clone(),
            conversation_id: cid.clone(),
            content,
            speaker_id: None,
        };
        let generate = Request::Generate {
            session_token: self.token.clone(),
            conversation_id: cid,
            speaker_id: speaker,
            parent_id: None,
        };
        let _ = self.commands.send(Command::SendThenGenerate {
            message: Box::new(message),
            generate: Box::new(generate),
        });
    }
    fn selected_speaker(&self) -> Option<String> {
        self.selected_conversation
            .as_ref()
            .and_then(|id| self.conversations.iter().find(|c| &c.id == id))
            .and_then(|c| c.participant_ids.first().cloned())
    }
    pub(super) fn render_new_chat_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.new_chat_open;
        let mut conversation_created = false;
        let max_height = Self::popup_max_height(ctx);
        let dialog_width = (ctx.content_rect().width() - 32.0).clamp(300.0, 520.0);
        egui::Window::new("New chat")
            .frame(modal_frame(ctx, self.light_mode, self.glass_mode))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(dialog_width)
            .max_width(dialog_width)
            .max_height(max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("new-chat-popup-content")
                    .max_height((max_height - 118.0).max(120.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new("Choose a character for a private chat.").weak(),
                        );
                        ui.add_space(16.0);
                        ui.label(egui::RichText::new("CONVERSATION NAME").size(11.0).weak());
                        ui.add_sized(
                            [ui.available_width(), 42.0],
                            egui::TextEdit::singleline(&mut self.new_conversation_title)
                                .hint_text("Optional — we can name it for you"),
                        );
                        ui.add_space(14.0);
                        ui.label(egui::RichText::new("CHARACTERS").size(11.0).weak());
                        ui.add_space(4.0);
                        for c in &self.characters {
                            let selected = self.selected_characters.contains(&c.id);
                            let mut selector_clicked = false;
                            let response = egui::Frame::new()
                                .fill(if selected {
                                    egui::Color32::from_rgb(40, 43, 75)
                                } else {
                                    color_surface_raised(ui)
                                })
                                .stroke(egui::Stroke::new(
                                    1.0,
                                    if selected {
                                        COLOR_PRIMARY
                                    } else {
                                        color_border(ui)
                                    },
                                ))
                                .corner_radius(12.0)
                                .inner_margin(egui::Margin::symmetric(12, 8))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let portrait = c
                                            .default_image_id
                                            .as_deref()
                                            .and_then(|id| {
                                                c.images.iter().find(|image| image.id == id)
                                            });
                                        if let Some(image) = portrait {
                                            Self::character_portrait(
                                                ui,
                                                &image.id,
                                                Some(&image.data),
                                                &c.name,
                                                34.0,
                                            );
                                        } else {
                                            Self::avatar(ui, &c.name, 34.0);
                                        }
                                        ui.vertical(|ui| {
                                            ui.label(egui::RichText::new(&c.name).strong());
                                            let summary = if c.personality.trim().is_empty() {
                                                "Ready to chat"
                                            } else {
                                                c.personality.as_str()
                                            };
                                            ui.label(
                                                egui::RichText::new(summary).size(12.0).weak(),
                                            );
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                selector_clicked = ui
                                                    .radio(selected, "")
                                                    .on_hover_text(format!(
                                                        "Start a private chat with {}",
                                                        c.name
                                                    ))
                                                    .clicked();
                                            },
                                        );
                                    });
                                })
                                .response
                                .interact(egui::Sense::click());
                            if response.clicked() || selector_clicked {
                                self.selected_characters.clear();
                                self.selected_characters.insert(c.id.clone());
                            }
                            ui.add_space(6.0);
                        }
                    });
                ui.add_space(12.0);
                let ready = !self.selected_characters.is_empty();
                let create = ui.add_enabled(
                    ready,
                    egui::Button::new(egui::RichText::new("Create conversation").strong())
                        .fill(COLOR_PRIMARY_STRONG)
                        .corner_radius(11.0)
                        .min_size(egui::vec2(ui.available_width(), 44.0)),
                );
                if create.clicked() {
                    let ids = self
                        .selected_characters
                        .iter()
                        .next()
                        .cloned()
                        .into_iter()
                        .collect::<Vec<_>>();
                    let title = if self.new_conversation_title.trim().is_empty() {
                        self.characters
                            .iter()
                            .find(|c| ids.contains(&c.id))
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "New chat".into())
                    } else {
                        self.new_conversation_title.clone()
                    };
                    self.send(Request::CreateConversation {
                        session_token: self.token.clone(),
                        title,
                        kind: ConversationKind::Direct,
                        participant_ids: ids,
                    });
                    conversation_created = true;
                    self.new_conversation_title.clear();
                    self.selected_characters.clear();
                }
            });
        self.new_chat_open = open && !conversation_created;
    }
}
