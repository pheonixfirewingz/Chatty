//! Conversation list and footer actions.

use crate::app::{ChattyApp, Message};
use crate::ui;
use crate::ui::{FooterIcon, sidebar_panel, sidebar_toggle_button};
use chatty_protocol::Role;
use iced::widget::{
    Id, Space, button, column, container, mouse_area, responsive, row, scrollable, stack, text,
    tooltip,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Theme};

impl ChattyApp {
    pub(super) fn sidebar_view<'a>(&'a self, compact: bool, short: bool) -> Element<'a, Message> {
        let header = row![
            text("Chatty").size(22).font(ui::BOLD),
            Space::new().width(Length::Fill),
            ui::ghost_icon_button(
                ui::VectorIcon::new(ui::MessageIcon::NewChat),
                Some("New chat"),
                Message::OpenNewChat,
            ),
            Space::new().width(12),
            sidebar_toggle_button(
                true,
                if compact {
                    Message::ToggleCompactSidebar
                } else {
                    Message::ToggleSidebar
                }
            ),
        ]
        .align_y(Alignment::Center);

        let recent_label = text("RECENT CHATS").size(11).color(ui::SECONDARY_TEXT);

        let mut conv_col = column![].spacing(4).width(Length::Fill);
        for conv in &self.conversations {
            let is_selected = self.selected_conversation.as_deref() == Some(&conv.id);
            let is_hovered = self.hovered_conversation_id.as_deref() == Some(&conv.id);
            let conv_id = conv.id.clone();
            let conv_del_id = conv.id.clone();
            let conv_hover_id = conv.id.clone();

            let title_btn = button(
                container(text(&conv.title).size(14).wrapping(text::Wrapping::None))
                    .align_y(Alignment::Center)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .on_press(Message::SelectConversation(conv_id))
            .height(Length::Fixed(38.0))
            .padding(Padding::from([0, 10]))
            .width(Length::Fill)
            .style(move |theme: &Theme, status: button::Status| {
                if is_selected {
                    button::Style {
                        background: Some(Background::Color(ui::COLOR_PRIMARY_STRONG)),
                        text_color: Color::WHITE,
                        border: Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: 8.0.into(),
                        },
                        ..Default::default()
                    }
                } else {
                    let bg = match status {
                        button::Status::Hovered => Color::from_rgba(
                            theme.palette().text.r,
                            theme.palette().text.g,
                            theme.palette().text.b,
                            0.08,
                        ),
                        button::Status::Pressed => Color::from_rgba(
                            theme.palette().text.r,
                            theme.palette().text.g,
                            theme.palette().text.b,
                            0.14,
                        ),
                        _ => Color::TRANSPARENT,
                    };
                    button::Style {
                        background: if bg == Color::TRANSPARENT {
                            None
                        } else {
                            Some(Background::Color(bg))
                        },
                        text_color: theme.palette().text,
                        border: Border {
                            color: Color::TRANSPARENT,
                            width: 0.0,
                            radius: 8.0.into(),
                        },
                        ..Default::default()
                    }
                }
            });

            let tile_item: Element<'_, Message> = if is_hovered {
                let tile_bg = if is_selected {
                    ui::COLOR_PRIMARY_STRONG
                } else {
                    let base = ui::surface_color(self.glass_mode);
                    let overlay = Color::WHITE;
                    Color::from_rgb(
                        base.r * 0.92 + overlay.r * 0.08,
                        base.g * 0.92 + overlay.g * 0.08,
                        base.b * 0.92 + overlay.b * 0.08,
                    )
                };

                let delete_btn = button(
                    container(text("×").size(16))
                        .center_x(Length::Fill)
                        .center_y(Length::Fill),
                )
                .on_press(Message::DeleteConversation(conv_del_id))
                .width(26)
                .height(26)
                .padding(0)
                .style(move |_theme: &Theme, status: button::Status| {
                    let (bg, border_color, text_color) = match status {
                        button::Status::Hovered => (
                            Color::from_rgba(0.95, 0.2, 0.2, 0.25),
                            Color::from_rgba(0.95, 0.2, 0.2, 0.50),
                            Color::from_rgb(1.0, 0.35, 0.35),
                        ),
                        button::Status::Pressed => (
                            Color::from_rgba(0.95, 0.2, 0.2, 0.40),
                            Color::from_rgba(0.95, 0.2, 0.2, 0.70),
                            Color::from_rgb(1.0, 0.45, 0.45),
                        ),
                        _ => (
                            Color::TRANSPARENT,
                            Color::TRANSPARENT,
                            if is_selected {
                                Color::from_rgba(1.0, 1.0, 1.0, 0.85)
                            } else {
                                Color::from_rgba(0.95, 0.35, 0.35, 0.85)
                            },
                        ),
                    };
                    button::Style {
                        background: if bg == Color::TRANSPARENT {
                            None
                        } else {
                            Some(Background::Color(bg))
                        },
                        text_color,
                        border: Border {
                            color: border_color,
                            width: 0.0,
                            radius: 6.0.into(),
                        },
                        ..Default::default()
                    }
                });

                let delete_tooltip = tooltip(
                    delete_btn,
                    container(text("Delete chat").size(12))
                        .padding(Padding::from([4, 8]))
                        .style(ui::tooltip_style),
                    tooltip::Position::Top,
                );

                let fade_box = container(
                    row![
                        Space::new().width(Length::Fill),
                        delete_tooltip,
                        Space::new().width(6),
                    ]
                    .align_y(Alignment::Center)
                    .height(Length::Fill),
                )
                .width(68)
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(tile_bg)),
                    border: Border {
                        radius: iced::border::Radius {
                            top_left: 0.0,
                            top_right: 8.0,
                            bottom_right: 8.0,
                            bottom_left: 0.0,
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                });

                let overlay = row![Space::new().width(Length::Fill), fade_box,]
                    .width(Length::Fill)
                    .height(Length::Fill);

                let tile_stack = stack![title_btn, overlay,];

                mouse_area(tile_stack)
                    .on_enter(Message::HoverConversation(Some(conv_hover_id)))
                    .on_exit(Message::HoverConversation(None))
                    .into()
            } else {
                let tile_stack = stack![title_btn,];

                mouse_area(tile_stack)
                    .on_enter(Message::HoverConversation(Some(conv_hover_id)))
                    .on_exit(Message::HoverConversation(None))
                    .into()
            };

            conv_col = conv_col.push(tile_item);
        }

        let conv_list = scrollable(conv_col)
            .id(Id::new("conversation-list"))
            .height(if short { Length::Shrink } else { Length::Fill })
            .width(Length::Fill);

        let status_row = row![text(&self.status).size(12).color(ui::SECONDARY_TEXT)];

        let mut actions = vec![
            (
                FooterIcon::Characters,
                Some("Manage characters"),
                Message::OpenCharactersDialog,
            ),
            (
                FooterIcon::World,
                Some("Manage world lore"),
                Message::OpenWorldsDialog,
            ),
            (
                FooterIcon::Memory,
                Some("Manage character memories"),
                Message::OpenMemoriesDialog(None, None),
            ),
        ];

        if self.role == Some(Role::Admin) {
            actions.push((
                FooterIcon::Admin,
                Some("Open admin portal"),
                Message::OpenAdminDialog,
            ));
        }

        actions.extend([
            (
                FooterIcon::Settings,
                Some("Open settings"),
                Message::OpenSettingsDialog,
            ),
            (FooterIcon::SignOut, Some("Sign out"), Message::SignOut),
        ]);

        let icons = responsive(move |size| {
            let (button_size, columns) = sidebar_icon_layout(size.width, actions.len());
            let mut rows = column![].spacing(6);
            for group in actions.chunks(columns) {
                let mut icons_row = row![].spacing(6).align_y(Alignment::Center);
                for (icon, label, message) in group {
                    icons_row = icons_row.push(ui::sized_icon_button(
                        *icon,
                        *label,
                        message.clone(),
                        button_size,
                    ));
                }
                rows = rows.push(icons_row);
            }
            rows.into()
        })
        .height(Length::Shrink);

        let sidebar_content = column![
            header,
            Space::new().height(10),
            recent_label,
            Space::new().height(4),
            conv_list,
            Space::new().height(8),
            status_row,
            Space::new().height(6),
            icons,
        ]
        .spacing(4)
        .padding(12)
        .width(if compact {
            Length::Fill
        } else {
            Length::Fixed(248.0)
        })
        .height(Length::Fill);

        let sidebar_content: Element<'a, Message> = if short {
            scrollable(sidebar_content.height(Length::Shrink)).into()
        } else {
            sidebar_content.into()
        };
        sidebar_panel(sidebar_content, self.glass_mode)
            .height(Length::Fill)
            .into()
    }
}

/// Shrink sidebar actions to 32px before switching to groups of three.
pub(in crate::app) fn sidebar_icon_layout(width: f32, count: usize) -> (f32, usize) {
    let single_row_size = (width - 6.0 * (count - 1) as f32) / count as f32;
    if single_row_size >= 32.0 {
        (single_row_size.min(44.0), count)
    } else {
        (((width - 12.0) / 3.0).clamp(32.0, 44.0), 3)
    }
}
