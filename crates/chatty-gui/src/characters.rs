use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};

impl From<&Character> for DraftCharacter {
    fn from(c: &Character) -> Self {
        Self {
            id: Some(c.id.clone()),
            name: c.name.clone(),
            description: c.description.clone(),
            personality: c.personality.clone(),
            scenario: c.scenario.clone(),
            system_prompt: c.system_prompt.clone(),
            example_dialogue: c.example_dialogue.clone(),
            appearance: c.appearance.clone(),
            age: c.age.clone(),
            gender: c.gender.clone(),
            race: c.race.clone(),
            misc: c.misc.clone(),
            tags: c.tags.join(", "),
            avatar: c.avatar.clone(),
            is_public: c.is_public,
            owned_by_user: c.owned_by_user,
        }
    }
}

impl ChattyApp {
    pub(super) fn render_character_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.draft_character_open;
        let max_height = Self::popup_max_height(ctx);
        let dialog_width = (ctx.content_rect().width() - 32.0).clamp(360.0, 920.0);
        egui::Window::new("Characters")
            .frame(modal_frame(ctx, self.light_mode, self.glass_mode))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([dialog_width, max_height.min(700.0)])
            .max_width(dialog_width)
            .max_height(max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let compact = ui.available_width() < 700.0;
                let pane_height = (max_height - 48.0).max(80.0);
                if compact {
                    let list_height = (pane_height * 0.24).clamp(96.0, 150.0);
                    egui::ScrollArea::vertical()
                        .id_salt("character-list-pane")
                        .max_height(list_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.character_list(ui));
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("character-editor-pane")
                        .max_height((pane_height - list_height - 12.0).max(80.0))
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.character_editor(ui));
                } else {
                    let total_width = ui.available_width();
                    let list_width = ((total_width - 12.0) * 0.24).clamp(190.0, 250.0);
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(list_width, pane_height),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("character-list-pane")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| self.character_list(ui));
                            },
                        );
                        ui.separator();
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), pane_height),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                egui::ScrollArea::vertical()
                                    .id_salt("character-editor-pane")
                                    .auto_shrink([false, false])
                                    .show(ui, |ui| self.character_editor(ui));
                            },
                        );
                    });
                }
            });
        self.draft_character_open = open;
    }
    fn character_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("New").clicked() {
                self.draft = DraftCharacter {
                    owned_by_user: true,
                    ..Default::default()
                };
            }
            if ui.button("Import").clicked() {
                self.import_character();
            }
        });
        ui.add_space(6.0);
        for c in self.characters.clone() {
            if ui
                .selectable_label(
                    self.draft.id.as_deref() == Some(&c.id),
                    format!("{}{}", c.name, if c.is_public { " · Public" } else { "" }),
                )
                .clicked()
            {
                self.draft = DraftCharacter::from(&c);
            }
        }
    }
    fn character_editor(&mut self, ui: &mut egui::Ui) {
        let can_edit = self.draft.owned_by_user;
        ui.heading(match (self.draft.id.is_some(), can_edit) {
            (false, _) => "Create character",
            (true, true) => "Edit character",
            (true, false) => "View character",
        });
        if !can_edit {
            ui.label("Public character · Only the owner can edit it.");
        }
        self.character_actions(ui);
        ui.separator();
        ui.add_enabled_ui(can_edit, |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut self.draft.name).desired_width(f32::INFINITY));
            ui.label("Description");
            ui.add(
                egui::TextEdit::multiline(&mut self.draft.description).desired_width(f32::INFINITY),
            );
            ui.label("Personality");
            ui.add(
                egui::TextEdit::multiline(&mut self.draft.personality).desired_width(f32::INFINITY),
            );
            ui.label("Appearance");
            ui.add(
                egui::TextEdit::multiline(&mut self.draft.appearance).desired_width(f32::INFINITY),
            );
            ui.horizontal_wrapped(|ui| {
                ui.allocate_ui(egui::vec2(ui.available_width() / 3.0, 0.0), |ui| {
                    ui.label("Age");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.age)
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.allocate_ui(egui::vec2(ui.available_width() / 3.0, 0.0), |ui| {
                    ui.label("Gender");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.gender)
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.allocate_ui(egui::vec2(ui.available_width(), 0.0), |ui| {
                    ui.label("Race");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.race)
                            .desired_width(f32::INFINITY),
                    );
                });
            });
            ui.label("Misc");
            ui.add(egui::TextEdit::multiline(&mut self.draft.misc).desired_width(f32::INFINITY));
            ui.label("Scenario");
            ui.add(
                egui::TextEdit::multiline(&mut self.draft.scenario).desired_width(f32::INFINITY),
            );
            ui.label("System prompt");
            ui.add(
                egui::TextEdit::multiline(&mut self.draft.system_prompt)
                    .desired_width(f32::INFINITY),
            );
            ui.label("Example dialogue");
            ui.add(
                egui::TextEdit::multiline(&mut self.draft.example_dialogue)
                    .desired_width(f32::INFINITY),
            );
            ui.label("Tags");
            ui.add(egui::TextEdit::singleline(&mut self.draft.tags).desired_width(f32::INFINITY));
        });
    }
    fn character_actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("Chat").clicked() {
                if let Some(id) = self.draft.id.clone() {
                    self.selected_characters.clear();
                    self.selected_characters.insert(id);
                    self.new_chat_open = true;
                    self.draft_character_open = false;
                }
            }
            if self.draft.owned_by_user {
                ui.toggle_value(&mut self.draft.is_public, "Public");
                if ui.button("Save").clicked() {
                    self.save_character();
                }
                if ui.button("Export").clicked() {
                    self.export_character();
                }
                if ui.button("Delete").clicked() {
                    if let Some(id) = self.draft.id.clone() {
                        self.send(Request::DeleteEntity {
                            session_token: self.token.clone(),
                            kind: EntityKind::Character,
                            entity_id: id,
                        });
                        self.draft = DraftCharacter {
                            owned_by_user: true,
                            ..Default::default()
                        };
                    }
                }
            }
        });
    }
    fn save_character(&mut self) {
        if self.draft.name.trim().is_empty() {
            self.set_error("Character name is required.");
            return;
        }
        self.send(Request::UpsertCharacter {
            session_token: self.token.clone(),
            character: CharacterInput {
                id: self.draft.id.clone(),
                name: self.draft.name.trim().into(),
                description: self.draft.description.clone(),
                personality: self.draft.personality.clone(),
                scenario: self.draft.scenario.clone(),
                system_prompt: self.draft.system_prompt.clone(),
                example_dialogue: self.draft.example_dialogue.clone(),
                appearance: self.draft.appearance.clone(),
                age: self.draft.age.clone(),
                gender: self.draft.gender.clone(),
                race: self.draft.race.clone(),
                misc: self.draft.misc.clone(),
                tags: self
                    .draft
                    .tags
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
                avatar: self.draft.avatar.clone(),
                is_public: self.draft.is_public,
                owned_by_user: self.draft.owned_by_user,
            },
        });
    }
    fn import_character(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Character card", &["json", "png"])
            .pick_file()
        else {
            return;
        };
        match std::fs::read(&path)
            .ok()
            .and_then(|bytes| {
                if path
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("png"))
                {
                    png_card_json(&bytes)
                } else {
                    Some(bytes)
                }
            })
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(draft_from_card)
        {
            Some(d) => self.draft = d,
            None => self.set_error("Could not read that SillyTavern card."),
        }
    }
    fn export_character(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(format!("{}.json", self.draft.name.replace(' ', "_")))
            .save_file()
        else {
            return;
        };
        let card = serde_json::json!({"spec":"chara_card_v2","spec_version":"2.0","data":{"name":self.draft.name,"description":self.draft.description,"personality":self.draft.personality,"scenario":self.draft.scenario,"first_mes":"","mes_example":self.draft.example_dialogue,"system_prompt":self.draft.system_prompt,"tags":self.draft.tags.split(',').map(str::trim).collect::<Vec<_>>(),"extensions":{"chatty":{"appearance":self.draft.appearance,"age":self.draft.age,"gender":self.draft.gender,"race":self.draft.race,"misc":self.draft.misc}}}});
        if let Ok(bytes) = serde_json::to_vec_pretty(&card) {
            if let Err(e) = std::fs::write(path, bytes) {
                self.set_error(format!("Export failed: {e}"));
            }
        }
    }
}

fn draft_from_card(root: serde_json::Value) -> Option<DraftCharacter> {
    let d = root.get("data").unwrap_or(&root);
    let ext = d.pointer("/extensions/chatty");
    Some(DraftCharacter {
        id: None,
        name: d.get("name")?.as_str()?.into(),
        description: str_field(d, "description"),
        personality: str_field(d, "personality"),
        scenario: str_field(d, "scenario"),
        system_prompt: str_field(d, "system_prompt"),
        example_dialogue: str_field(d, "mes_example"),
        appearance: ext.map(|e| str_field(e, "appearance")).unwrap_or_default(),
        age: ext.map(|e| str_field(e, "age")).unwrap_or_default(),
        gender: ext.map(|e| str_field(e, "gender")).unwrap_or_default(),
        race: ext.map(|e| str_field(e, "race")).unwrap_or_default(),
        misc: ext.map(|e| str_field(e, "misc")).unwrap_or_default(),
        tags: d
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        avatar: None,
        is_public: false,
        owned_by_user: true,
    })
}
fn str_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .into()
}

fn png_card_json(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        if at + 12 + len > bytes.len() {
            return None;
        }
        if kind == b"tEXt" {
            let data = &bytes[at + 8..at + 8 + len];
            if let Some(zero) = data.iter().position(|b| *b == 0) {
                if &data[..zero] == b"chara" {
                    return STANDARD.decode(&data[zero + 1..]).ok();
                }
            }
        }
        at += 12 + len;
    }
    None
}
