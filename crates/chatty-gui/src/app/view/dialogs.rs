//! Settings, certificate trust, and notice dialogs.

use crate::app::{ChattyApp, Message, UiNotice};
use crate::network::PendingCertificate;
use crate::ui;
use iced::widget::{Space, button, column, container, row, text, toggler};
use iced::{Element, Length, font};

impl ChattyApp {
    pub(super) fn settings_dialog_view<'a>(&'a self) -> Element<'a, Message> {
        let title = text("Settings").size(22).font(ui::BOLD);

        let speech_header = text("Local Speech (TTS)").size(14).font(ui::SEMIBOLD);
        let auto_speak_toggle = toggler(self.speech.auto_speak)
            .label("Automatically read new replies")
            .on_toggle(Message::SetTtsAutoSpeak);
        let dialogue_only_toggle = toggler(self.speech.dialogue_only)
            .label("Read only quoted dialogue")
            .on_toggle(Message::SetTtsDialogueOnly);

        let test_voice_btn = button(text("Test voice").size(13))
            .on_press(Message::TestVoice)
            .padding([6, 12])
            .style(ui::secondary_button_style());

        let usage_header = text("Account Token Usage").size(14).font(ui::SEMIBOLD);
        let usage_txt = text(format!(
            "Prompt: {} · Completion: {} · Total: {}",
            self.account_usage.prompt_tokens,
            self.account_usage.completion_tokens,
            self.account_usage.prompt_tokens + self.account_usage.completion_tokens
        ))
        .size(13);

        let close_btn = button(text("Close").size(14).font(ui::BOLD))
            .on_press(Message::CloseSettingsDialog)
            .padding([8, 16])
            .style(ui::primary_button_style());

        let content = column![
            title,
            Space::new().height(10),
            speech_header,
            auto_speak_toggle,
            dialogue_only_toggle,
            test_voice_btn,
            Space::new().height(12),
            usage_header,
            usage_txt,
            Space::new().height(16),
            close_btn,
        ]
        .spacing(6)
        .padding(20)
        .max_width(400);

        container(content).width(Length::Fill).into()
    }

    pub(super) fn certificate_trust_dialog_view<'a>(
        &'a self,
        cert: &'a PendingCertificate,
    ) -> Element<'a, Message> {
        let title = text("Trust this Chatty server?").size(20).font(ui::BOLD);

        let desc = text(format!(
            "{} does not have a certificate trusted by the public internet.",
            cert.target.server_name
        ))
        .size(13);

        let subject = if cert.is_ca() {
            "private CA"
        } else {
            "server certificate"
        };

        let fp_desc = text(format!(
            "Confirm this {subject} SHA-256 fingerprint with the server administrator before continuing:"
        ))
        .size(12);

        let fp_text = text(&cert.fingerprint).size(11).font(font::Font::MONOSPACE);

        let trust_btn = button(text("Trust and connect").size(14).font(ui::BOLD))
            .on_press(Message::TrustCertificate(true))
            .padding(10)
            .style(ui::primary_button_style());

        let cancel_btn = button(text("Cancel").size(14))
            .on_press(Message::TrustCertificate(false))
            .padding(10)
            .style(ui::secondary_button_style());

        let btn_row = row![trust_btn, cancel_btn].spacing(12);

        let content = column![
            title,
            desc,
            Space::new().height(6),
            fp_desc,
            fp_text,
            Space::new().height(12),
            btn_row,
        ]
        .spacing(6)
        .padding(20)
        .max_width(460);

        container(content).width(Length::Fill).into()
    }

    pub(super) fn error_modal_view<'a>(&'a self, err: &'a UiNotice) -> Element<'a, Message> {
        let title = text("Notice").size(18).font(ui::BOLD);
        let time_txt = text(&err.timestamp).size(11).color(ui::SECONDARY_TEXT);
        let msg_txt = text(&err.message).size(13);

        let close_btn = button(text("Close").size(13))
            .on_press(Message::DismissError)
            .padding([6, 14])
            .style(ui::secondary_button_style());

        let content = column![
            title,
            time_txt,
            Space::new().height(4),
            msg_txt,
            Space::new().height(10),
            close_btn,
        ]
        .spacing(4)
        .padding(16)
        .max_width(400);

        container(content).padding(16).width(Length::Fill).into()
    }
}
