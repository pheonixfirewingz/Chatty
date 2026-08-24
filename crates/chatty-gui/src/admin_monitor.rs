use super::*;

impl ChattyApp {
    pub(super) fn load_admin_tab(&mut self) {
        match self.admin_tab {
            0 => {
                self.send(Request::AdminGetBrokerConfig {
                    session_token: self.token.clone(),
                });
                self.send(Request::AdminGetBrokerMonitor {
                    session_token: self.token.clone(),
                });
                self.last_monitor_refresh = Some(Instant::now());
            }
            1 => self.send(Request::AdminListUsers {
                session_token: self.token.clone(),
            }),
            2 => self.send(Request::AdminGetOllamaState {
                session_token: self.token.clone(),
            }),
            _ => self.send(Request::AdminReadDatabase {
                session_token: self.token.clone(),
            }),
        }
    }
    pub(super) fn refresh_admin_monitor_if_due(&mut self, ctx: &egui::Context) {
        if self.screen != Screen::Admin || self.admin_tab != 0 || self.token.is_empty() {
            self.last_monitor_refresh = None;
            return;
        }
        let interval = Duration::from_secs(2);
        ctx.request_repaint_after(interval);
        if self
            .last_monitor_refresh
            .is_none_or(|last| last.elapsed() >= interval)
        {
            self.send(Request::AdminGetBrokerMonitor {
                session_token: self.token.clone(),
            });
            self.last_monitor_refresh = Some(Instant::now());
        }
    }
    pub(super) fn render_admin_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.screen == Screen::Admin;
        let max_height = Self::popup_max_height(ctx);
        egui::Window::new("Admin")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([820.0, max_height.min(620.0)])
            .max_height(max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (index, label) in ["Broker", "Users", "Ollama", "Data"].iter().enumerate() {
                        if ui
                            .selectable_label(self.admin_tab == index, *label)
                            .clicked()
                        {
                            self.admin_tab = index;
                            self.load_admin_tab();
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("admin-popup-content")
                    .max_height((max_height - 72.0).max(80.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| match self.admin_tab {
                        0 => self.render_admin_broker(ui),
                        1 => self.render_admin_users(ui),
                        2 => self.render_admin_ollama(ui),
                        _ => self.render_admin_data(ui),
                    });
            });
        if !open {
            self.screen = Screen::Chat;
        }
    }
    fn render_admin_broker(&mut self, ui: &mut egui::Ui) {
        ui.heading("Monitoring");
        if let Some(m) = &self.broker_monitor {
            let memory = match m.memory_limit_mb {
                Some(limit) => format!("{} / {} MB", m.memory_used_mb, limit),
                None => format!("{} MB", m.memory_used_mb),
            };
            let adapter = match m.adapter_status {
                AdapterStatus::Disabled => "Disabled".to_owned(),
                AdapterStatus::Online => format!(
                    "Online · {} models · {} ms",
                    m.adapter_model_count,
                    m.adapter_latency_ms.unwrap_or_default()
                ),
                AdapterStatus::Offline => "Offline".to_owned(),
            };
            if ui.available_width() < 520.0 {
                egui::Grid::new("compact-monitor-metrics")
                    .num_columns(2)
                    .spacing([16.0, 5.0])
                    .show(ui, |ui| {
                        compact_metric(ui, "Uptime", format_duration(m.uptime_seconds));
                        compact_metric(ui, "CPU", format!("{:.1}%", m.cpu_percent));
                        compact_metric(ui, "Memory", memory);
                        compact_metric(ui, "Connections", m.active_connections.to_string());
                        compact_metric(ui, "Adapter", adapter);
                    });
            } else {
                ui.horizontal_wrapped(|ui| {
                    metric(ui, "Uptime", format_duration(m.uptime_seconds));
                    metric(ui, "CPU", format!("{:.1}%", m.cpu_percent));
                    metric(ui, "Memory", memory);
                    metric(ui, "Connections", m.active_connections.to_string());
                    metric(ui, "Adapter", adapter);
                });
            }
            if !m.recent_errors.is_empty() {
                ui.add_space(8.0);
                ui.strong("Recent errors");
                for e in &m.recent_errors {
                    ui.colored_label(egui::Color32::LIGHT_RED, e);
                }
            }
        } else {
            ui.label("Monitoring data has not loaded.");
        }
        ui.horizontal(|ui| {
            if ui.button("Refresh").clicked() {
                self.send(Request::AdminGetBrokerMonitor {
                    session_token: self.token.clone(),
                });
            }
            if ui
                .button("Reboot")
                .on_hover_text("Graceful soft reboot")
                .clicked()
            {
                self.send(Request::AdminSoftReboot {
                    session_token: self.token.clone(),
                });
                let _ = self.commands.send(Command::Reconnect);
            }
        });
        ui.separator();
        ui.heading("Adapter");
        ui.horizontal(|ui| {
            Self::toggle_switch(ui, &mut self.broker_config.adapter_enabled, "Enabled");
            ui.label("Enabled");
        });
        ui.label("URL");
        ui.add(
            egui::TextEdit::singleline(&mut self.broker_config.adapter_url)
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            Self::toggle_switch(
                ui,
                &mut self.broker_config.use_ollama_api,
                "Ollama native API",
            );
            ui.label("Ollama native API").on_hover_text(
                "Uses /api/chat so context, Top K, repeat penalty, and keep-alive are honored. Leave off for generic OpenAI-compatible servers.",
            );
        });
        ui.label("Model (blank = first available)");
        let model_names = self
            .ollama_state
            .as_ref()
            .map(|state| {
                state
                    .models
                    .iter()
                    .map(|model| model.name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        egui::ComboBox::from_id_salt("adapter-model")
            .selected_text(if self.broker_config.model.is_empty() {
                "Automatic"
            } else {
                &self.broker_config.model
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.broker_config.model, String::new(), "Automatic");
                for model in model_names {
                    ui.selectable_value(&mut self.broker_config.model, model.clone(), model);
                }
            });
        ui.heading("Generation defaults");
        egui::Grid::new("generation-defaults")
            .num_columns(4)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Temperature");
                ui.add(
                    egui::DragValue::new(&mut self.broker_config.temperature)
                        .range(0.0..=2.0)
                        .speed(0.01),
                );
                ui.label("Top P");
                ui.add(
                    egui::DragValue::new(&mut self.broker_config.top_p)
                        .range(0.0..=1.0)
                        .speed(0.01),
                );
                ui.end_row();
                ui.label("Top K");
                ui.add(egui::DragValue::new(&mut self.broker_config.top_k).range(0..=10_000));
                ui.label("Context");
                ui.add(
                    egui::DragValue::new(&mut self.broker_config.num_ctx).range(128..=1_048_576),
                );
                ui.end_row();
                ui.label("Max tokens");
                ui.add(
                    egui::DragValue::new(&mut self.broker_config.num_predict).range(-1..=1_048_576),
                );
                ui.label("Repeat penalty");
                ui.add(
                    egui::DragValue::new(&mut self.broker_config.repeat_penalty)
                        .range(0.0..=2.0)
                        .speed(0.01),
                );
                ui.end_row();
                ui.label("Seed");
                ui.add(egui::DragValue::new(&mut self.broker_config.seed));
                ui.label("Keep alive");
                ui.text_edit_singleline(&mut self.broker_config.keep_alive);
                ui.end_row();
            });
        ui.heading("Policy");
        ui.horizontal(|ui| {
            Self::toggle_switch(
                ui,
                &mut self.broker_config.allow_public_characters,
                "Public characters",
            );
            ui.label("Public characters");
        });
        ui.horizontal(|ui| {
            Self::toggle_switch(
                ui,
                &mut self.broker_config.allow_self_registration,
                "Registration",
            );
            ui.label("Registration");
        });
        if ui.button("Save").clicked() {
            self.send(Request::AdminSetBrokerConfig {
                session_token: self.token.clone(),
                config: self.broker_config.clone(),
            });
        }
    }
    fn render_admin_ollama(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Ollama server");
            if ui.button("Refresh").clicked() {
                self.send(Request::AdminGetOllamaState {
                    session_token: self.token.clone(),
                });
            }
        });
        let Some(state) = self.ollama_state.clone() else {
            ui.label(
                "Ollama data has not loaded. Confirm the adapter URL points to an Ollama server.",
            );
            return;
        };
        ui.weak(format!(
            "Version {} · {} installed · {} running",
            state.version,
            state.models.len(),
            state.running_models.len()
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.ollama_pull_model)
                    .hint_text("Model, e.g. llama3.2:3b")
                    .desired_width(320.0),
            );
            if ui.button("Pull").clicked() && !self.ollama_pull_model.trim().is_empty() {
                self.send(Request::AdminOllamaAction {
                    session_token: self.token.clone(),
                    action: OllamaAction::Pull {
                        model: self.ollama_pull_model.trim().to_owned(),
                    },
                });
            }
        });
        ui.separator();
        ui.strong("Installed models");
        for model in state.models {
            let running = state
                .running_models
                .iter()
                .any(|item| item.name == model.name);
            ui.horizontal_wrapped(|ui| {
                ui.label(&model.name);
                ui.weak(format!(
                    "{} · {} · {} · {}",
                    human_bytes(model.size),
                    model.family,
                    model.parameter_size,
                    model.quantization_level
                ));
                if ui.button(if running { "Reload" } else { "Load" }).clicked() {
                    self.send(Request::AdminOllamaAction {
                        session_token: self.token.clone(),
                        action: OllamaAction::Load {
                            model: model.name.clone(),
                        },
                    });
                }
                if running && ui.button("Unload").clicked() {
                    self.send(Request::AdminOllamaAction {
                        session_token: self.token.clone(),
                        action: OllamaAction::Unload {
                            model: model.name.clone(),
                        },
                    });
                }
                if ui.button("Delete").clicked() {
                    self.send(Request::AdminOllamaAction {
                        session_token: self.token.clone(),
                        action: OllamaAction::Delete { model: model.name },
                    });
                }
            });
        }
        if !state.running_models.is_empty() {
            ui.separator();
            ui.strong("Runtime allocation");
            for model in state.running_models {
                ui.label(format!(
                    "{} · VRAM {} · expires {}",
                    model.name,
                    human_bytes(model.size_vram),
                    model.expires_at
                ));
            }
        }
    }
    fn render_admin_users(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.admin_new_username);
            ui.add(
                egui::TextEdit::singleline(&mut self.admin_new_password)
                    .password(true)
                    .hint_text("Password"),
            );
            egui::ComboBox::from_id_salt("new-role")
                .selected_text(format!("{:?}", self.admin_new_role))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.admin_new_role, Role::User, "User");
                    ui.selectable_value(&mut self.admin_new_role, Role::Admin, "Admin");
                });
            if ui.button("Create").clicked() {
                self.send(Request::AdminCreateUser {
                    session_token: self.token.clone(),
                    username: self.admin_new_username.clone(),
                    password: self.admin_new_password.clone(),
                    role: self.admin_new_role,
                });
                self.admin_new_password.clear();
            }
        });
        ui.separator();
        for user in self.users.clone() {
            ui.horizontal(|ui| {
                ui.label(&user.username);
                ui.weak(format!("{:?}", user.role));
                if user.id != self.user_id {
                    let role = if user.role == Role::Admin {
                        Role::User
                    } else {
                        Role::Admin
                    };
                    if ui
                        .button(if role == Role::Admin {
                            "Promote"
                        } else {
                            "Demote"
                        })
                        .clicked()
                    {
                        self.send(Request::AdminSetRole {
                            session_token: self.token.clone(),
                            user_id: user.id.clone(),
                            role,
                        });
                    }
                    if ui.button("Delete").clicked() {
                        self.send(Request::AdminDeleteUser {
                            session_token: self.token.clone(),
                            user_id: user.id,
                        });
                    }
                }
            });
        }
    }
    fn render_admin_data(&mut self, ui: &mut egui::Ui) {
        if ui.button("Refresh").clicked() {
            self.load_admin_tab();
        }
        egui::Grid::new("admin-data").striped(true).show(ui, |ui| {
            ui.strong("Type");
            ui.strong("Name");
            ui.strong("Details");
            ui.end_row();
            for row in self.admin_data.clone() {
                ui.label(&row.kind);
                ui.label(&row.label);
                ui.label(&row.detail);
                if let Some(mut public) = row.is_public {
                    if ui.toggle_value(&mut public, "Public").changed() {
                        self.send(Request::AdminSetCharacterPublic {
                            session_token: self.token.clone(),
                            character_id: row.id,
                            is_public: public,
                        });
                    }
                }
                ui.end_row();
            }
        });
    }
}
fn metric(ui: &mut egui::Ui, label: &str, value: String) {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .corner_radius(8.0)
        .inner_margin(10.0)
        .show(ui, |ui| {
            ui.weak(label);
            ui.strong(value);
        });
}
fn compact_metric(ui: &mut egui::Ui, label: &str, value: String) {
    ui.weak(label);
    ui.strong(value);
    ui.end_row();
}
fn format_duration(s: u64) -> String {
    let h = s / 3600;
    let m = (s % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", s % 60)
    }
}
fn human_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else {
        format!("{:.0} MiB", bytes as f64 / MIB)
    }
}
