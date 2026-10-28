//! Admin portal dialog: tab bar plus the broker, users, logs, and data tabs.

use super::model_manager::ollama_tab_view;
use super::*;
use crate::ui;
use chatty_protocol::{AdapterStatus, Role};
use iced::widget::{
    Space, button, column, container, pick_list, row, rule, scrollable, text, text_input, toggler,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Theme};

pub const ADMIN_TAB_NAMES: [&str; 6] = ["Broker", "Users", "Models", "Images", "Logs", "Data"];

/// Renders the complete Admin modal dialog including tab bar and active tab content.
pub fn admin_dialog_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    // Modal header
    let header = row![
        text("Admin Portal").size(18),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding(Padding::from([2, 8]))
            .on_press(Message::from(AdminMessage::CloseDialog)),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill);

    // Tab navigation buttons
    let mut tab_row = row![].spacing(6).align_y(Alignment::Center);
    for (idx, name) in ADMIN_TAB_NAMES.iter().enumerate() {
        let is_selected = state.admin_tab == idx;
        let tab_btn = button(text(*name).size(13).color(if is_selected {
            ui::COLOR_ON_PRIMARY
        } else {
            ui::SECONDARY_TEXT
        }))
        .padding(Padding::from([6, 14]))
        .style(move |theme: &Theme, status: button::Status| {
            let (bg, border_color) = if is_selected {
                (ui::COLOR_PRIMARY_STRONG, ui::COLOR_PRIMARY_STRONG)
            } else {
                match status {
                    button::Status::Hovered => (
                        Color::from_rgba(1.0, 1.0, 1.0, 0.12),
                        Color::from_rgba(1.0, 1.0, 1.0, 0.22),
                    ),
                    button::Status::Pressed => (
                        Color::from_rgba(1.0, 1.0, 1.0, 0.18),
                        Color::from_rgba(1.0, 1.0, 1.0, 0.30),
                    ),
                    _ => (
                        Color::from_rgba(1.0, 1.0, 1.0, 0.05),
                        Color::from_rgba(1.0, 1.0, 1.0, 0.10),
                    ),
                }
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: if is_selected {
                    ui::COLOR_ON_PRIMARY
                } else {
                    theme.palette().text
                },
                border: Border {
                    color: border_color,
                    width: 0.0,
                    radius: 6.0.into(),
                },
                shadow: iced::Shadow::default(),
                snap: false,
            }
        })
        .on_press(Message::from(AdminMessage::SelectTab(idx)));

        tab_row = tab_row.push(tab_btn);
    }

    // Active tab body
    let tab_content: Element<'a, Message> = match state.admin_tab {
        0 => broker_tab_view(state),
        1 => users_tab_view(state),
        2 => ollama_tab_view(state),
        3 => image_settings::view(state),
        4 => logs_tab_view(state),
        _ => data_tab_view(state),
    };

    let scroll = scrollable(tab_content)
        .width(Length::Fill)
        .height(Length::Fill);

    let content_col = column![header, tab_row.wrap(), rule::horizontal(1), scroll]
        .spacing(12)
        .width(Length::Fill)
        .height(Length::Fill);

    container(content_col)
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// Helper creating a standardized metric display card.
pub(super) fn metric_card<'a, Message: 'a>(
    label: &'static str,
    val: String,
    glass_mode: bool,
) -> Element<'a, Message> {
    let content = column![
        text(label).size(12).color(ui::SECONDARY_TEXT),
        text(val).size(15),
    ]
    .spacing(3);

    container(content)
        .padding(Padding::from([8, 12]))
        .style(move |_| ui::card_style(glass_mode))
        .into()
}

/// Broker Tab: broker metrics display, recent errors, adapter & generation parameter settings.
pub fn broker_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![].spacing(14).width(Length::Fill);

    // 1. Monitoring Section
    items = items.push(text("Monitoring").size(16));

    if let Some(m) = state.broker_monitor {
        let mem_str = match m.memory_limit_mb {
            Some(limit) => format!("{} / {} MB", m.memory_used_mb, limit),
            None => format!("{} MB", m.memory_used_mb),
        };
        let adapter_str = match m.adapter_status {
            AdapterStatus::Disabled => "Disabled".to_string(),
            AdapterStatus::Online => format!(
                "Online · {} models · {} ms",
                m.adapter_model_count,
                m.adapter_latency_ms.unwrap_or_default()
            ),
            AdapterStatus::Offline => "Offline".to_string(),
        };

        let metrics_row = row![
            metric_card(
                "Uptime",
                format_duration(m.uptime_seconds),
                state.glass_mode
            ),
            metric_card("CPU", format!("{:.1}%", m.cpu_percent), state.glass_mode),
            metric_card("Memory", mem_str, state.glass_mode),
            metric_card(
                "Connections",
                m.active_connections.to_string(),
                state.glass_mode
            ),
            metric_card("Adapter", adapter_str, state.glass_mode),
        ]
        .spacing(8);

        items = items.push(metrics_row.wrap());

        if !m.recent_errors.is_empty() {
            let mut err_col = column![text("Recent errors:").size(13)].spacing(4);
            for err in &m.recent_errors {
                err_col = err_col.push(
                    text(format!("• {err}"))
                        .size(12)
                        .color(Color::from_rgb(0.95, 0.3, 0.3)),
                );
            }
            items = items.push(err_col);
        }
    } else {
        items = items.push(
            text("Monitoring data has not loaded.")
                .size(13)
                .color(Color::from_rgba(0.6, 0.6, 0.6, 1.0)),
        );
    }

    let monitor_actions = row![
        button(text("Refresh")).on_press(Message::from(AdminMessage::RefreshMonitor)),
        button(text("Soft Reboot")).on_press(Message::from(AdminMessage::SoftReboot)),
    ]
    .spacing(8);
    items = items.push(monitor_actions.wrap());

    items = items.push(rule::horizontal(1));

    items = items
        .push(button(text("Model settings")).on_press(Message::from(AdminMessage::ModelsPage(2))));

    // 4. Policy Section
    items = items.push(text("Policy").size(16));

    let public_chars = toggler(state.broker_config.allow_public_characters)
        .label("Allow public characters")
        .on_toggle(|v| Message::from(AdminMessage::TogglePublicCharacters(v)));

    let registration = toggler(state.broker_config.allow_self_registration)
        .label("Allow self-registration")
        .on_toggle(|v| Message::from(AdminMessage::ToggleRegistration(v)));

    items = items.push(public_chars);
    items = items.push(registration);

    let save_btn = button(text("Save policy").size(14))
        .padding(Padding::from([6, 16]))
        .on_press(Message::from(AdminMessage::SaveBrokerConfig));

    items = items.push(save_btn);

    items.into()
}

/// Users Tab: user creation form, user cards with token metrics, promote/demote and delete buttons.
pub fn users_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![].spacing(14).width(Length::Fill);

    // 1. Create User Section
    items = items.push(text("Create User").size(16));

    let username_input = text_input("Username", state.admin_new_username)
        .on_input(|s| Message::from(AdminMessage::NewUsernameChanged(s)))
        .width(Length::Fixed(180.0));

    let password_input = text_input("Password", state.admin_new_password)
        .secure(true)
        .on_input(|s| Message::from(AdminMessage::NewPasswordChanged(s)))
        .width(Length::Fixed(180.0));

    let role_picker = pick_list(
        &[AdminRoleOption::User, AdminRoleOption::Admin][..],
        Some(AdminRoleOption::from(state.admin_new_role)),
        |r: AdminRoleOption| Message::from(AdminMessage::NewRoleChanged(r.into())),
    )
    .width(Length::Fixed(120.0));

    let create_btn = button(text("Create"))
        .padding(Padding::from([5, 12]))
        .on_press(Message::from(AdminMessage::CreateUser));

    let create_row = row![username_input, password_input, role_picker, create_btn]
        .spacing(8)
        .align_y(Alignment::Center);

    items = items.push(create_row.wrap());
    items = items.push(rule::horizontal(1));

    // 2. User Cards Section
    items = items.push(text(format!("Users ({})", state.users.len())).size(16));

    if state.users.is_empty() {
        items = items.push(
            text("No users")
                .size(13)
                .color(Color::from_rgba(0.6, 0.6, 0.6, 1.0)),
        );
    } else {
        for user in state.users {
            let role_label = match user.role {
                Role::Admin => "Admin",
                Role::User => "User",
            };

            let mut card_row = row![
                text(&user.username).size(14),
                text(role_label)
                    .size(12)
                    .color(Color::from_rgba(0.5, 0.5, 0.5, 1.0)),
                hint(
                    text(format!("{} tokens", format_token_count(user.usage.total()))).size(12),
                    format!(
                        "{} prompt · {} completion",
                        format_token_count(user.usage.prompt_tokens),
                        format_token_count(user.usage.completion_tokens)
                    )
                ),
                Space::new().width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center);

            if user.id != state.current_user_id {
                let (next_role, action_label) = if user.role == Role::Admin {
                    (Role::User, "Demote")
                } else {
                    (Role::Admin, "Promote")
                };

                let user_id = user.id.clone();
                let role_btn = button(text(action_label).size(12))
                    .padding(Padding::from([3, 8]))
                    .on_press(Message::from(AdminMessage::SetUserRole {
                        user_id: user_id.clone(),
                        role: next_role,
                    }));

                let del_btn = button(text("Delete").size(12))
                    .padding(Padding::from([3, 8]))
                    .on_press(Message::from(AdminMessage::DeleteUser(user_id)));

                card_row = card_row.push(role_btn).push(del_btn);
            }

            let card = ui::card(card_row.wrap(), state.glass_mode);
            items = items.push(card);
        }
    }

    items.into()
}

pub(super) fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
    let mut style = button::primary(theme, status);
    style.text_color = ui::COLOR_ON_PRIMARY;
    style
}

/// Keep optional guidance out of the default layout.
pub(super) fn hint<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    help: impl Into<std::borrow::Cow<'a, str>>,
) -> Element<'a, Message> {
    let help: std::borrow::Cow<'a, str> = help.into();
    iced::widget::tooltip(
        content,
        container(text(help).size(12))
            .padding(8)
            .max_width(300)
            .style(ui::tooltip_style),
        iced::widget::tooltip::Position::Top,
    )
    .into()
}

/// Logs Tab: broker log viewer with search and size overview.
pub fn logs_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![].spacing(10).width(Length::Fill);

    let mut header_actions = row![
        text("Broker Log").size(16),
        Space::new().width(Length::Fill),
        button(text("Refresh")).on_press(Message::from(AdminMessage::RefreshLogs)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    if !state.admin_log.content.is_empty() {
        header_actions = header_actions
            .push(button(text("Copy all")).on_press(Message::from(AdminMessage::CopyLogs)));
    }

    items = items.push(header_actions);

    items = items.push(hint(
        text("Private chat data · current boot")
            .size(12)
            .color(ui::SECONDARY_TEXT),
        "Includes AI prompts and private chat data. Restarting the broker clears this log.",
    ));

    let search_bar = text_input("Filter log lines...", state.admin_log_search)
        .on_input(|s| Message::from(AdminMessage::SearchLogsChanged(s)))
        .width(Length::Fill);
    items = items.push(search_bar);

    let size_str = human_bytes(state.admin_log.file_size_bytes);
    let size_label = text(if state.admin_log.truncated {
        format!("{size_str} on disk · showing newest 512 KiB")
    } else {
        format!("{size_str} on disk · complete current-boot log")
    })
    .size(11)
    .color(Color::from_rgba(0.5, 0.5, 0.5, 1.0));
    items = items.push(size_label);

    let filter = state.admin_log_search.trim().to_lowercase();
    let matching_lines: Vec<&str> = if filter.is_empty() {
        state.admin_log.content.lines().collect()
    } else {
        state
            .admin_log
            .content
            .lines()
            .filter(|line| line.to_lowercase().contains(&filter))
            .collect()
    };

    let log_view: Element<'a, Message> = if matching_lines.is_empty() {
        text(if filter.is_empty() {
            "No entries"
        } else {
            "No matches"
        })
        .size(12)
        .into()
    } else {
        let mut lines_col = column![].spacing(2).width(Length::Fill);
        for line in matching_lines {
            lines_col = lines_col.push(text(line).font(iced::Font::MONOSPACE).size(11));
        }
        lines_col.into()
    };

    let glass_mode = state.glass_mode;
    let log_box = container(log_view)
        .padding(10)
        .width(Length::Fill)
        .style(move |_| ui::card_style(glass_mode));

    items = items.push(log_box);

    items.into()
}

/// Data Tab: database tables explorer with character public toggles.
pub fn data_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let mut items = column![
        row![
            text(format!("{} records", state.admin_data.len())).size(16),
            Space::new().width(Length::Fill),
            button(text("Refresh")).on_press(Message::from(AdminMessage::RefreshData)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
    ]
    .spacing(10)
    .width(Length::Fill);
    for record in state.admin_data {
        let mut title = row![
            text(&record.label).size(15).wrapping(text::Wrapping::Glyph),
            text(&record.kind).size(12).color(ui::SECONDARY_TEXT),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if let Some(is_public) = record.is_public {
            let character_id = record.id.clone();
            title = title.push(
                toggler(is_public)
                    .label("Public")
                    .on_toggle(move |is_public| {
                        Message::from(AdminMessage::ToggleCharacterPublic {
                            character_id: character_id.clone(),
                            is_public,
                        })
                    }),
            );
        }
        let card = column![
            title.wrap(),
            text(&record.detail)
                .size(13)
                .wrapping(text::Wrapping::Glyph)
        ]
        .spacing(6);
        items = items.push(ui::card(card, state.glass_mode));
    }
    if state.admin_data.is_empty() {
        items = items.push(text("No records").size(13));
    }
    items.into()
}
