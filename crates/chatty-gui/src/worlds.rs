use super::*;

impl ChattyApp {
    pub(super) fn render_world_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.worlds_open;
        egui::Window::new("World lore")
            .frame(modal_frame(ctx, self.light_mode, self.glass_mode))
            .open(&mut open).collapsible(false)
            .default_size([720.0, Self::popup_max_height(ctx).min(760.0)])
            .max_width((ctx.content_rect().width() - 32.0).max(240.0))
            .max_height(Self::popup_max_height(ctx))
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    egui::ComboBox::from_id_salt("world-picker")
                        .selected_text(if self.world_draft.id.is_empty() { "New world" } else { &self.world_draft.name })
                        .show_ui(ui, |ui| {
                            for world in &self.worlds {
                                if ui.selectable_label(self.world_draft.id == world.id, &world.name).clicked() {
                                    self.world_draft = world.clone();
                                }
                            }
                        });
                    if ui.button("New world").clicked() { self.world_draft = World::default(); }
                    if ui.add_enabled(
                        !self.world_import_pending,
                        egui::Button::new(if self.world_import_pending { "Importing…" } else { "Import SillyTavern" }),
                    ).on_hover_text("Use the configured AI model to convert a SillyTavern lorebook into an editable preview").clicked() {
                        self.import_silly_tavern_world();
                    }
                    if ui.button("Save world").clicked() {
                        match self.world_draft.validate() {
                            Err(error) => self.set_error(error),
                            Ok(()) => {
                                if self.world_draft.id.is_empty() { self.world_draft.id = chatty_protocol::util::new_uuid(); }
                                self.send(Request::SaveWorld { session_token: self.token.clone(), world: self.world_draft.clone() });
                            }
                        }
                    }
                    if ui.add_enabled(!self.world_draft.id.is_empty(), egui::Button::new("Delete world")).clicked() {
                        self.send(Request::DeleteWorld { session_token: self.token.clone(), world_id: self.world_draft.id.clone() });
                        self.world_draft = World::default();
                    }
                });
                ui.label(if self.worlds.iter().any(|world| world == &self.world_draft) {
                    "Saved"
                } else { "Unsaved changes — save to apply" });
                if self.world_import_pending {
                    ctx.request_repaint_after(std::time::Duration::from_secs(1));
                    let elapsed = self
                        .world_import_started
                        .map_or(0, |started| started.elapsed().as_secs());
                    ui.horizontal_wrapped(|ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new(format!(
                                "Converting lore with AI… {elapsed}s elapsed"
                            ))
                            .strong()
                            .color(egui::Color32::from_rgb(100, 180, 255)),
                        );
                    });
                    ui.label("The model is reading, classifying, and formatting every entry. Large lorebooks or a cold model can take several minutes.");
                } else if let Some(notice) = &self.world_import_notice {
                    ui.label(egui::RichText::new(notice).color(egui::Color32::from_rgb(100, 180, 255)));
                }
                ui.separator();
                egui::ScrollArea::vertical().id_salt("world-editor").auto_shrink([false, false]).show(ui, |ui| {
                    let label = ui.label("World name");
                    ui.add(egui::TextEdit::singleline(&mut self.world_draft.name).desired_width(f32::INFINITY)).labelled_by(label.id);
                    ui.label("Linked characters");
                    ui.label("This world is available when a linked character speaks in your chats.");
                    ui.horizontal_wrapped(|ui| {
                        for character in &self.characters {
                            let mut linked = self.world_draft.character_ids.contains(&character.id);
                            if ui.checkbox(&mut linked, &character.name).changed() {
                                self.world_draft.character_ids.retain(|id| id != &character.id);
                                if linked { self.world_draft.character_ids.push(character.id.clone()); }
                            }
                        }
                    });
                    for id in self.world_draft.character_ids.clone() {
                        if !self.characters.iter().any(|character| character.id == id) {
                            let mut linked = true;
                            if ui.push_id(&id, |ui| ui.checkbox(&mut linked, "Unavailable character (uncheck to unlink)")).inner.changed() {
                                self.world_draft.character_ids.retain(|linked_id| linked_id != &id);
                            }
                        }
                    }
                    ui.separator();
                    ui.label("Common knowledge is selected first. Other facts activate from keywords in the last six messages. Priority decides what fits within the context budget.");
                    if ui.button("Add lore entry").clicked() {
                        self.world_draft.entries.push(WorldFact { enabled: true, ..Default::default() });
                    }
                    let mut remove = None;
                    for (index, fact) in self.world_draft.entries.iter_mut().enumerate() {
                        ui.push_id(index, |ui| {
                            ui.group(|ui| {
                                ui.set_min_width((ui.available_width() - 8.0).max(0.0));
                                ui.horizontal_wrapped(|ui| {
                                    ui.checkbox(&mut fact.enabled, "Enabled");
                                    ui.checkbox(&mut fact.common_knowledge, "Common knowledge");
                                    ui.label("Priority");
                                    ui.add(egui::DragValue::new(&mut fact.priority).range(-1000..=1000));
                                    if ui.button("Remove").clicked() { remove = Some(index); }
                                });
                                let label = ui.label("Title");
                                ui.add(egui::TextEdit::singleline(&mut fact.title).desired_width(f32::INFINITY)).labelled_by(label.id);
                                if !fact.common_knowledge {
                                    let label = ui.label("Keywords (comma separated, case insensitive)");
                                    let mut keywords = fact.keywords.join(",");
                                    if ui.add(egui::TextEdit::singleline(&mut keywords).desired_width(f32::INFINITY)).labelled_by(label.id).changed() {
                                        fact.keywords = keywords.split(',').map(str::to_owned).collect();
                                    }
                                }
                                let label = ui.label("Lore content");
                                ui.add(egui::TextEdit::multiline(&mut fact.content).desired_rows(3).desired_width(f32::INFINITY)).labelled_by(label.id);
                            });
                        });
                    }
                    if let Some(index) = remove { self.world_draft.entries.remove(index); }
                });
            });
        self.worlds_open = open;
    }

    fn import_silly_tavern_world(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("SillyTavern lorebook", &["json"])
            .pick_file()
        else {
            return;
        };
        let result = std::fs::read(&path).map_err(|error| format!("Could not read lorebook: {error}"))
            .and_then(|bytes| {
                if bytes.len() > 2 * 1024 * 1024 {
                    Err("SillyTavern lorebook exceeds 2 MiB".into())
                } else {
                    String::from_utf8(bytes).map_err(|_| "Lorebook is not valid UTF-8 JSON".into())
                }
            })
            .and_then(|text| {
                serde_json::from_str::<serde_json::Value>(&text)
                    .map_err(|error| format!("Lorebook is not valid JSON: {error}"))?;
                Ok(text)
            });
        match result {
            Ok(lorebook_json) => {
                let source_name = path.file_stem().and_then(|name| name.to_str())
                    .unwrap_or("Imported world").replace(['_', '-'], " ");
                self.world_import_pending = true;
                self.world_import_started = Some(Instant::now());
                self.world_import_notice = None;
                self.send(Request::ImportSillyTavernWorld {
                    session_token: self.token.clone(),
                    source_name,
                    lorebook_json,
                });
            }
            Err(error) => self.set_error(error),
        }
    }
}
