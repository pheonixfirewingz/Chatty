//! Connect, session-restore, and sign-in screens.

use crate::app::{ChattyApp, Message};
use crate::ui;
use iced::widget::{Space, button, column, container, scrollable, text, text_input};
use iced::{Alignment, Element, Length};

impl ChattyApp {
    pub(super) fn server_connection_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Connect to Chatty").size(28).font(ui::BOLD);
        let subtitle = text("Enter the IP address or domain of your Chatty server.")
            .size(14)
            .color(ui::SECONDARY_TEXT);

        let addr_input = text_input("192.168.0.98 or chatty.example.com", &self.server_address)
            .on_input(Message::ServerAddressChanged)
            .on_submit(Message::TryConnect)
            .padding(10)
            .width(Length::Fill);

        let connect_btn = button(
            text(if self.connecting {
                "Trying connection…"
            } else {
                "Try connection"
            })
            .size(15)
            .font(ui::BOLD),
        )
        .on_press(Message::TryConnect)
        .padding(10)
        .width(Length::Fill)
        .style(ui::primary_button_style());

        let status_txt = text(&self.status).size(13).color(ui::SECONDARY_TEXT);

        let card_content = column![
            title,
            subtitle,
            Space::new().height(16),
            text("Server address").size(13),
            addr_input,
            Space::new().height(8),
            connect_btn,
            Space::new().height(6),
            status_txt,
        ]
        .spacing(6)
        .max_width(420)
        .align_x(Alignment::Center);

        self.centered_card(card_content)
    }

    pub(super) fn session_restore_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Signing you in").size(28).font(ui::BOLD);
        let status_txt = text(&self.status).size(14);

        let card_content = column![title, Space::new().height(12), status_txt]
            .spacing(8)
            .align_x(Alignment::Center);

        self.centered_card(card_content)
    }

    pub(super) fn login_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Welcome back").size(28).font(ui::BOLD);
        let status_txt = text(&self.status).size(13).color(ui::SECONDARY_TEXT);

        let username_input = text_input("Username", &self.username)
            .on_input(Message::LoginUsernameChanged)
            .on_submit(Message::SubmitLogin)
            .padding(10)
            .width(Length::Fill);

        let password_input = text_input("Password", &self.password)
            .on_input(Message::LoginPasswordChanged)
            .on_submit(Message::SubmitLogin)
            .secure(true)
            .padding(10)
            .width(Length::Fill);

        let sign_in_btn = button(text("Sign in").size(15).font(ui::BOLD))
            .on_press(Message::SubmitLogin)
            .padding(10)
            .width(Length::Fill)
            .style(ui::primary_button_style());

        let mut card_col = column![
            title,
            status_txt,
            Space::new().height(14),
            text("Username").size(13),
            username_input,
            text("Password").size(13),
            password_input,
            Space::new().height(6),
            sign_in_btn,
        ]
        .spacing(6)
        .max_width(420)
        .align_x(Alignment::Center);

        if self.registration_enabled {
            let register_btn = button(text("Register").size(14))
                .on_press(Message::SubmitRegister)
                .padding(8)
                .width(Length::Fill)
                .style(ui::secondary_button_style());
            card_col = card_col.push(register_btn);
        }

        let change_server_btn = button(text("Change server").size(13))
            .on_press(Message::ChangeServer)
            .padding(6)
            .style(ui::secondary_button_style());
        card_col = card_col
            .push(Space::new().height(6))
            .push(change_server_btn);

        self.centered_card(card_col)
    }

    /// A glass card centred in the window, scrolling when the window is short.
    fn centered_card<'a>(&self, content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
        container(scrollable(
            container(ui::card(content, self.glass_mode).max_width(460))
                .padding(16)
                .center_x(Length::Fill),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }
}
