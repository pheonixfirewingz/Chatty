use super::*;

impl ChattyApp {
    fn character_name(&self, id: Option<&str>) -> String {
        id.and_then(|id| self.characters.iter().find(|c| c.id == id))
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Assistant".into())
    }
    pub(super) fn render_chat(&mut self, ui: &mut egui::Ui) {
        if !self.sidebar_visible && ui.button("Chats").clicked() {
            self.sidebar_visible = true;
        }
        let available = ui.available_rect_before_wrap();
        let typing_height = if self.typing_character.is_some() {
            26.0
        } else {
            0.0
        };
        let composer_height = 66.0;
        let messages_rect = egui::Rect::from_min_max(
            available.min,
            egui::pos2(
                available.max.x,
                (available.max.y - composer_height - typing_height).max(available.min.y),
            ),
        );
        let composer_rect = egui::Rect::from_min_max(
            egui::pos2(available.min.x, available.max.y - composer_height),
            available.max,
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(composer_rect), |ui| {
            egui::Frame::new()
                .fill(egui::Color32::from_rgba_unmultiplied(30, 33, 42, 220))
                .corner_radius(16.0)
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let response = ui.add_sized(
                            [(ui.available_width() - 70.0).max(100.0), 44.0],
                            egui::TextEdit::multiline(&mut self.input)
                                .hint_text("Message…")
                                .desired_rows(1),
                        );
                        let enter = response.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                        if let Some(id) = self.active_request {
                            if ui.button("Stop").clicked() {
                                let _ = self.commands.send(Command::Cancel(id));
                            }
                        } else {
                            let label = if self.input.trim().is_empty() {
                                "Continue"
                            } else {
                                "Send"
                            };
                            if ui.button(label).clicked() || enter {
                                self.submit_message();
                            }
                        }
                    });
                });
        });
        if let Some(id) = self.typing_character.as_deref() {
            let typing_rect = egui::Rect::from_min_max(
                egui::pos2(available.min.x, messages_rect.max.y),
                egui::pos2(available.max.x, composer_rect.min.y),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(typing_rect), |ui| {
                ui.label(
                    egui::RichText::new(format!("{} is typing…", self.character_name(Some(id))))
                        .italics()
                        .weak(),
                );
            });
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(messages_rect), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("chat-messages")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add_space(12.0);
                    if self.selected_conversation.is_none() {
                        ui.vertical_centered(|ui| {
                            ui.add_space(ui.available_height() * 0.3);
                            if ui.button("Continue").clicked() {
                                self.new_chat_open = true;
                            }
                        });
                        return;
                    }
                    for message in self.messages.clone() {
                        ui.push_id(&message.id, |ui| self.render_message(ui, &message));
                        ui.add_space(14.0);
                    }
                    if !self.stream_text.is_empty() {
                        let name = self.character_name(self.typing_character.as_deref());
                        ui.strong(egui::RichText::new(name).size(20.0));
                        let text = self.stream_text.clone();
                        self.render_markdown(ui, &text);
                    }
                });
        });
    }
    fn render_message(&mut self, ui: &mut egui::Ui, message: &ChatMessage) {
        let user = message.author_type == "user";
        let rendered = ui.scope(|ui| {
            if user {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgb(48, 52, 65))
                        .corner_radius(16.0)
                        .inner_margin(egui::Margin::symmetric(13, 9))
                        .show(ui, |ui| {
                            ui.set_max_width((ui.available_width() * 0.72).max(120.0));
                            ui.label(&message.content);
                        });
                });
            } else {
                ui.horizontal(|ui| {
                    ui.strong(
                        egui::RichText::new(self.character_name(message.author_id.as_deref()))
                            .size(20.0),
                    );
                    if ui.small_button("↻").on_hover_text("Regenerate").clicked() {
                        self.regenerate(message);
                    }
                });
                self.render_markdown(ui, &message.content);
            }
        });
        let message_rect = rendered.response.rect;
        let hovered = ui
            .input(|input| input.pointer.hover_pos())
            .is_some_and(|pointer| message_rect.contains(pointer));
        let delete_rect = egui::Rect::from_min_size(
            egui::pos2(message_rect.right() - 26.0, message_rect.top()),
            egui::vec2(24.0, 24.0),
        );
        if hovered
            && ui
                .put(delete_rect, egui::Button::new("×").frame(false))
                .on_hover_text("Delete")
                .clicked()
        {
            self.delete_message(&message.id);
        }
    }
    fn delete_message(&self, id: &str) {
        self.send(Request::DeleteEntity {
            session_token: self.token.clone(),
            kind: EntityKind::Message,
            entity_id: id.into(),
        });
    }
    fn regenerate(&self, message: &ChatMessage) {
        if let Some(cid) = self.selected_conversation.clone() {
            self.send(Request::Generate {
                session_token: self.token.clone(),
                conversation_id: cid,
                speaker_id: message.author_id.clone(),
                parent_id: message.parent_id.clone(),
            });
        }
    }
    fn submit_message(&mut self) {
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
                parent_id: self.messages.last().map(|m| m.id.clone()),
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
        let max_height = Self::popup_max_height(ctx);
        egui::Window::new("New chat")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .max_height(max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("new-chat-popup-content")
                    .max_height((max_height - 48.0).max(80.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.label("Title");
                        ui.text_edit_singleline(&mut self.new_conversation_title);
                        ui.label("Characters");
                        for c in &self.characters {
                            let mut selected = self.selected_characters.contains(&c.id);
                            if ui.checkbox(&mut selected, &c.name).changed() {
                                if selected {
                                    self.selected_characters.insert(c.id.clone());
                                } else {
                                    self.selected_characters.remove(&c.id);
                                }
                            }
                        }
                        if self.selected_characters.len() > 1 {
                            egui::ComboBox::from_label("Mode")
                                .selected_text(match self.new_conversation_kind {
                                    ConversationKind::GroupManual => "Manual",
                                    ConversationKind::GroupRoundRobin => "Round robin",
                                    ConversationKind::GroupAutomatic => "Automatic",
                                    _ => "Manual",
                                })
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.new_conversation_kind,
                                        ConversationKind::GroupManual,
                                        "Manual",
                                    );
                                    ui.selectable_value(
                                        &mut self.new_conversation_kind,
                                        ConversationKind::GroupRoundRobin,
                                        "Round robin",
                                    );
                                    ui.selectable_value(
                                        &mut self.new_conversation_kind,
                                        ConversationKind::GroupAutomatic,
                                        "Automatic",
                                    );
                                });
                        }
                        if ui.button("Create").clicked() {
                            let ids = self.selected_characters.iter().cloned().collect::<Vec<_>>();
                            let kind = if ids.len() > 1 {
                                self.new_conversation_kind
                            } else {
                                ConversationKind::Direct
                            };
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
                                kind,
                                participant_ids: ids,
                            });
                            self.new_chat_open = false;
                            self.new_conversation_title.clear();
                            self.selected_characters.clear();
                        }
                    });
            });
        self.new_chat_open = open;
    }
}
