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
                    for (index, label) in ["Broker", "Users", "Data"].iter().enumerate() {
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
            ui.horizontal_wrapped(|ui| {
                metric(ui, "Uptime", format_duration(m.uptime_seconds));
                metric(ui, "CPU", format!("{:.1}%", m.cpu_percent));
                metric(
                    ui,
                    "Memory",
                    match m.memory_limit_mb {
                        Some(limit) => format!("{} / {} MB", m.memory_used_mb, limit),
                        None => format!("{} MB", m.memory_used_mb),
                    },
                );
                metric(ui, "Connections", m.active_connections.to_string());
                let adapter = match m.adapter_status {
                    AdapterStatus::Disabled => "Disabled".to_owned(),
                    AdapterStatus::Online => format!(
                        "Online · {} models · {} ms",
                        m.adapter_model_count,
                        m.adapter_latency_ms.unwrap_or_default()
                    ),
                    AdapterStatus::Offline => "Offline".to_owned(),
                };
                metric(ui, "Adapter", adapter);
            });
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
fn format_duration(s: u64) -> String {
    let h = s / 3600;
    let m = (s % 3600) / 60;
    if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {}s", s % 60)
    }
}
