use super::*;

impl ChattyApp {
    fn blank_memory(character_id: Option<String>, conversation_id: Option<String>) -> MemoryInput {
        MemoryInput {
            id: None,
            conversation_id,
            character_id,
            kind: MemoryKind::Fact,
            content: String::new(),
            importance: 50,
            pinned: false,
        }
    }

    pub(super) fn request_filtered_memories(&self) {
        self.send(Request::ListMemories {
            session_token: self.token.clone(),
            conversation_id: self.memory_conversation_filter.clone(),
            character_id: self.memory_character_filter.clone(),
        });
    }

    pub(super) fn open_memory_manager(
        &mut self,
        character_id: Option<String>,
        conversation_id: Option<String>,
    ) {
        self.memory_character_filter = character_id;
        self.memory_conversation_filter = conversation_id;
        self.memory_kind_filter = None;
        self.memory_search.clear();
        self.memory_editor_open = false;
        self.memory_delete_confirmation = None;
        self.memory_dialog_open = true;
        self.request_filtered_memories();
    }

    pub(super) fn render_memory_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.memory_dialog_open;
        let max_height = Self::popup_max_height(ctx);
        let dialog_width = (ctx.content_rect().width() - 24.0).clamp(300.0, 820.0);
        egui::Window::new("Character memories")
            .frame(modal_frame(ctx, self.light_mode, self.glass_mode))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([dialog_width, max_height.min(700.0)])
            .max_width(dialog_width)
            .max_height(max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("Review and control what your characters remember across conversations.");
                ui.add_space(6.0);
                self.memory_toolbar(ui);
                if let Some(notice) = &self.memory_notice {
                    ui.label(egui::RichText::new(notice).color(color_primary_text(ui)));
                }
                ui.separator();
                if self.memory_editor_open {
                    self.memory_editor(ui);
                } else {
                    self.memory_list(ui, max_height);
                }
            });
        self.memory_dialog_open = open;
        if self.memory_delete_confirmation.is_some() {
            self.memory_delete_dialog(ctx);
        }
    }

    fn memory_toolbar(&mut self, ui: &mut egui::Ui) {
        let compact = ui.available_width() < 620.0;
        let mut filters_changed = false;
        let search = |ui: &mut egui::Ui, this: &mut Self| {
            let label = ui.label("Search");
            ui.add(
                egui::TextEdit::singleline(&mut this.memory_search)
                    .hint_text("Memory text")
                    .desired_width(if compact { f32::INFINITY } else { 220.0 }),
            )
            .labelled_by(label.id);
        };
        if compact {
            search(ui, self);
            ui.horizontal_wrapped(|ui| {
                filters_changed |= self.memory_filter_controls(ui);
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                search(ui, self);
                filters_changed |= self.memory_filter_controls(ui);
            });
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Add memory").clicked() {
                self.memory_draft = Self::blank_memory(
                    self.memory_character_filter.clone(),
                    self.memory_conversation_filter.clone(),
                );
                self.memory_editor_open = true;
                self.memory_notice = None;
            }
            let can_learn = self.memory_conversation_filter.is_some();
            if ui
                .add_enabled(can_learn, egui::Button::new("Learn from chat"))
                .on_hover_text("Extract one durable memory from the selected conversation")
                .clicked()
            {
                self.send(Request::ExtractMemory {
                    session_token: self.token.clone(),
                    conversation_id: self.memory_conversation_filter.clone().unwrap_or_default(),
                    character_id: self.memory_character_filter.clone(),
                });
                self.memory_notice = Some("Learning from chat…".into());
            }
            if ui.button("Refresh").clicked() {
                self.request_filtered_memories();
            }
            if !self.memory_search.is_empty() && ui.button("Clear search").clicked() {
                self.memory_search.clear();
            }
        });
        if filters_changed {
            self.memory_editor_open = false;
            self.request_filtered_memories();
        }
    }

    fn memory_filter_controls(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        let selected_character = self
            .memory_character_filter
            .as_deref()
            .and_then(|id| self.characters.iter().find(|character| character.id == id))
            .map_or("All characters", |character| character.name.as_str());
        egui::ComboBox::from_id_salt("memory-character-filter")
            .selected_text(selected_character)
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut self.memory_character_filter, None, "All characters")
                    .changed();
                for character in &self.characters {
                    changed |= ui
                        .selectable_value(
                            &mut self.memory_character_filter,
                            Some(character.id.clone()),
                            &character.name,
                        )
                        .changed();
                }
            });
        let selected_conversation = self
            .memory_conversation_filter
            .as_deref()
            .and_then(|id| {
                self.conversations
                    .iter()
                    .find(|conversation| conversation.id == id)
            })
            .map_or("All conversations", |conversation| {
                conversation.title.as_str()
            });
        egui::ComboBox::from_id_salt("memory-conversation-filter")
            .selected_text(selected_conversation)
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(
                        &mut self.memory_conversation_filter,
                        None,
                        "All conversations",
                    )
                    .changed();
                for conversation in &self.conversations {
                    changed |= ui
                        .selectable_value(
                            &mut self.memory_conversation_filter,
                            Some(conversation.id.clone()),
                            &conversation.title,
                        )
                        .changed();
                }
            });
        egui::ComboBox::from_id_salt("memory-kind-filter")
            .selected_text(
                self.memory_kind_filter
                    .map_or("All types", MemoryKind::label),
            )
            .show_ui(ui, |ui| {
                changed |= ui
                    .selectable_value(&mut self.memory_kind_filter, None, "All types")
                    .changed();
                for kind in [
                    MemoryKind::Fact,
                    MemoryKind::Event,
                    MemoryKind::Relationship,
                    MemoryKind::Reflection,
                ] {
                    changed |= ui
                        .selectable_value(&mut self.memory_kind_filter, Some(kind), kind.label())
                        .changed();
                }
            });
        changed
    }

    fn memory_editor(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .fill(color_surface_raised(ui))
            .stroke(egui::Stroke::new(1.0, color_border(ui)))
            .corner_radius(10.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.heading(if self.memory_draft.id.is_some() {
                    "Edit memory"
                } else {
                    "Add memory"
                });
                let label = ui.label("What should the character remember?");
                ui.add(
                    egui::TextEdit::multiline(&mut self.memory_draft.content)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text("A durable fact, event, promise, or relationship change"),
                )
                .labelled_by(label.id);
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("memory-editor-kind")
                        .selected_text(self.memory_draft.kind.label())
                        .show_ui(ui, |ui| {
                            for kind in [
                                MemoryKind::Fact,
                                MemoryKind::Event,
                                MemoryKind::Relationship,
                                MemoryKind::Reflection,
                            ] {
                                ui.selectable_value(
                                    &mut self.memory_draft.kind,
                                    kind,
                                    kind.label(),
                                );
                            }
                        });
                    egui::ComboBox::from_id_salt("memory-editor-character")
                        .selected_text(
                            self.memory_draft
                                .character_id
                                .as_deref()
                                .and_then(|id| {
                                    self.characters.iter().find(|character| character.id == id)
                                })
                                .map_or("All characters", |character| character.name.as_str()),
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.memory_draft.character_id,
                                None,
                                "All characters",
                            );
                            for character in &self.characters {
                                ui.selectable_value(
                                    &mut self.memory_draft.character_id,
                                    Some(character.id.clone()),
                                    &character.name,
                                );
                            }
                        });
                    egui::ComboBox::from_id_salt("memory-editor-conversation")
                        .selected_text(
                            self.memory_draft
                                .conversation_id
                                .as_deref()
                                .and_then(|id| {
                                    self.conversations
                                        .iter()
                                        .find(|conversation| conversation.id == id)
                                })
                                .map_or("All conversations", |conversation| {
                                    conversation.title.as_str()
                                }),
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.memory_draft.conversation_id,
                                None,
                                "All conversations",
                            );
                            for conversation in &self.conversations {
                                ui.selectable_value(
                                    &mut self.memory_draft.conversation_id,
                                    Some(conversation.id.clone()),
                                    &conversation.title,
                                );
                            }
                        });
                });
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.memory_draft.pinned, "Always remember");
                    ui.label("Importance");
                    ui.add(egui::Slider::new(
                        &mut self.memory_draft.importance,
                        0..=100,
                    ));
                });
                ui.horizontal(|ui| {
                    let valid = !self.memory_draft.content.trim().is_empty();
                    if ui
                        .add_enabled(valid, egui::Button::new("Save memory"))
                        .clicked()
                    {
                        self.memory_draft.content = self.memory_draft.content.trim().to_owned();
                        self.send(Request::UpsertMemory {
                            session_token: self.token.clone(),
                            memory: self.memory_draft.clone(),
                        });
                        self.memory_notice = Some("Saving memory…".into());
                        self.memory_editor_open = false;
                    }
                    if ui.button("Cancel").clicked() {
                        self.memory_editor_open = false;
                    }
                });
            });
    }

    fn memory_list(&mut self, ui: &mut egui::Ui, max_height: f32) {
        let query = self.memory_search.trim().to_lowercase();
        let memories = self
            .memories
            .iter()
            .filter(|memory| {
                self.memory_kind_filter
                    .is_none_or(|kind| memory.kind == kind)
                    && self
                        .memory_character_filter
                        .as_deref()
                        .is_none_or(|id| memory.character_id.as_deref() == Some(id))
                    && self
                        .memory_conversation_filter
                        .as_deref()
                        .is_none_or(|id| memory.conversation_id.as_deref() == Some(id))
                    && (query.is_empty() || memory.content.to_lowercase().contains(&query))
            })
            .cloned()
            .collect::<Vec<_>>();
        if memories.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.label(if query.is_empty() {
                    "No memories yet. Add one or let the character learn during conversation."
                } else {
                    "No memories match this search."
                });
            });
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("memory-list")
            .max_height((max_height - 190.0).max(160.0))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for memory in memories {
                    self.memory_card(ui, memory);
                    ui.add_space(8.0);
                }
            });
    }

    fn memory_card(&mut self, ui: &mut egui::Ui, memory: MemoryEntry) {
        let character = memory
            .character_id
            .as_deref()
            .and_then(|id| self.characters.iter().find(|character| character.id == id))
            .map_or("All characters", |character| character.name.as_str());
        let conversation = memory
            .conversation_id
            .as_deref()
            .and_then(|id| {
                self.conversations
                    .iter()
                    .find(|conversation| conversation.id == id)
            })
            .map_or("All conversations", |conversation| {
                conversation.title.as_str()
            });
        let metadata = format!(
            "{} · {} · {} · {}",
            memory.kind.label(),
            character,
            conversation,
            memory.source.label()
        );
        egui::Frame::new()
            .fill(color_surface_raised(ui))
            .stroke(egui::Stroke::new(1.0, color_border(ui)))
            .corner_radius(10.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    if memory.pinned {
                        ui.label(
                            egui::RichText::new("Pinned")
                                .strong()
                                .color(color_primary_text(ui)),
                        );
                    }
                    ui.label(egui::RichText::new(metadata).size(12.0).weak());
                });
                ui.add_space(4.0);
                ui.label(&memory.content);
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Edit").clicked() {
                        self.memory_draft = MemoryInput {
                            id: Some(memory.id.clone()),
                            conversation_id: memory.conversation_id.clone(),
                            character_id: memory.character_id.clone(),
                            kind: memory.kind,
                            content: memory.content.clone(),
                            importance: memory.importance,
                            pinned: memory.pinned,
                        };
                        self.memory_editor_open = true;
                        self.memory_notice = None;
                    }
                    if ui.button("Delete").clicked() {
                        self.memory_delete_confirmation = Some(memory.id.clone());
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "Importance {} · Updated {}",
                            memory.importance, memory.updated_at
                        ))
                        .size(11.0)
                        .weak(),
                    );
                });
            });
    }

    fn memory_delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(id) = self.memory_delete_confirmation.clone() else {
            return;
        };
        let preview = self
            .memories
            .iter()
            .find(|memory| memory.id == id)
            .map(|memory| memory.content.clone())
            .unwrap_or_else(|| "This memory".into());
        egui::Window::new("Delete memory?")
            .frame(modal_frame(ctx, self.light_mode, self.glass_mode))
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label("The character will stop using this memory immediately.");
                ui.add_space(6.0);
                ui.label(egui::RichText::new(preview).italics());
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.memory_delete_confirmation = None;
                    }
                    if ui.button("Delete memory").clicked() {
                        self.send(Request::DeleteEntity {
                            session_token: self.token.clone(),
                            kind: EntityKind::Memory,
                            entity_id: id.clone(),
                        });
                        self.memory_notice = Some("Removing memory…".into());
                        self.memory_delete_confirmation = None;
                    }
                });
            });
    }
}
