//! Root view: the active screen plus the topmost modal.

mod auth;
mod dialogs;
mod sidebar;

#[cfg(test)]
pub(super) use sidebar::sidebar_icon_layout;

use crate::admin::admin_dialog_view;
use crate::app::{ChattyApp, Message, Screen};
use crate::characters::{ImageGeneration, character_dialog_view};
use crate::conversation::{
    ConversationMessage, ConversationViewState, NewChatViewState, conversation_view,
    new_chat_dialog_view,
};
use crate::memories::{MemoryViewState, memory_dialog_view};
use crate::ui;
use crate::worlds::{WorldViewState, world_dialog_view};
use iced::widget::{Space, container, responsive, row, scrollable};
use iced::{Background, Color, Element, Length, Size};
use iced_dialog::dialog;

impl ChattyApp {
    pub(crate) fn view<'a>(&'a self) -> Element<'a, Message> {
        responsive(move |size| self.view_at_size(size)).into()
    }

    pub(super) fn view_at_size<'a>(&'a self, size: Size) -> Element<'a, Message> {
        let base: Element<Message> = if !self.connected {
            self.server_connection_view()
        } else if self.restoring_session {
            self.session_restore_view()
        } else if self.token.is_empty() {
            self.login_view()
        } else {
            self.shell_view(size)
        };

        // Keep the base wrapped in the dialog stack even when no modal is open. Returning the
        // bare base would change the root widget type whenever a modal opens or closes, which
        // discards the widget tree state and resets every scroll offset to the top.
        let root: Element<Message> = match self.modal_view(size) {
            Some(modal) => {
                let glass_mode = self.glass_mode;
                let opaque = modal.opaque;
                dialog(true, base, modal.content)
                    .backdrop(if glass_mode {
                        Color::from_rgba(0.0, 0.0, 0.0, 130.0 / 255.0)
                    } else {
                        Color::from_rgba(0.0, 0.0, 0.0, 0.35)
                    })
                    .padding_inner(12)
                    .padding_outer(8)
                    .max_width(modal.max_width)
                    .max_height(modal.max_height)
                    .on_press(modal.on_close)
                    .container_style(move |_| {
                        let mut style = ui::modal_dialog_frame_style(glass_mode);
                        if opaque {
                            style.background = Some(Background::Color(ui::PALETTE.background));
                        }
                        style
                    })
                    .into()
            }
            None => dialog(false, base, Space::new()).into(),
        };

        let glass_mode = self.glass_mode;

        container(root)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| ui::window_backdrop_style(glass_mode))
            .into()
    }

    /// The topmost open modal, in priority order.
    fn modal_view<'a>(&'a self, size: Size) -> Option<Modal<'a>> {
        // Notices may cover the whole window; editors leave a margin and stop growing.
        let notice_height = (size.height - 16.0).max(1.0);
        let editor_height = |cap: f32| (size.height - 32.0).clamp(1.0, cap);
        let modal = |content: Element<'a, Message>, max_width, max_height, on_close| Modal {
            content,
            max_width,
            max_height,
            on_close,
            opaque: false,
        };
        let modal = if let Some(cert) = &self.pending_certificate {
            modal(
                scrollable(self.certificate_trust_dialog_view(cert)).into(),
                460.0,
                notice_height,
                Message::TrustCertificate(false),
            )
        } else if let Some(err) = &self.error {
            modal(
                scrollable(self.error_modal_view(err)).into(),
                400.0,
                notice_height,
                Message::DismissError,
            )
        } else if self.worlds_open {
            let state = WorldViewState {
                worlds: &self.worlds,
                world_draft: &self.world_state.world_draft,
                characters: &self.characters,
                entry_search: &self.world_state.entry_search,
                import_pending: self.world_state.import_pending,
                import_elapsed_secs: self
                    .world_state
                    .import_started
                    .map_or(0, |started| started.elapsed().as_secs()),
                import_notice: self.world_state.import_notice.as_deref(),
                glass_mode: self.glass_mode,
            };
            modal(
                world_dialog_view(&state),
                740.0,
                editor_height(780.0),
                Message::CloseWorldsDialog,
            )
        } else if self.memory_dialog_open {
            let can_extract =
                self.selected_conversation.is_some() && self.selected_speaker().is_some();
            let state = MemoryViewState::new(
                &self.memories,
                &self.characters,
                &self.conversations,
                &self.memory_state,
                can_extract,
                self.glass_mode,
            );
            modal(
                memory_dialog_view(&state),
                780.0,
                editor_height(780.0),
                Message::CloseMemoriesDialog,
            )
        } else if self.draft_character_open {
            let generation = match &self.emotion_batch {
                Some(batch) => ImageGeneration::Batch {
                    done: batch.done,
                    total: batch.total,
                },
                None => self.portrait_image.state(self.image_generation),
            };
            let content = character_dialog_view(
                &self.characters,
                &self.draft,
                &self.draft_editors,
                &self.draft_world_ids,
                &self.worlds,
                &self.character_search,
                self.char_import_pending,
                generation,
                self.glass_mode,
                size.width < 800.0,
            )
            .map(Message::Character);
            modal(
                content,
                940.0,
                editor_height(740.0),
                Message::CloseCharactersDialog,
            )
        } else if self.new_chat_open {
            let state = NewChatViewState {
                is_open: true,
                characters: &self.characters,
                selected_character_id: self.selected_characters.iter().next().map(String::as_str),
                title: &self.new_conversation_title,
                glass_mode: self.glass_mode,
            };
            modal(
                new_chat_dialog_view(&state, Message::Conversation)?,
                460.0,
                editor_height(580.0),
                Message::Conversation(ConversationMessage::CloseNewChatDialog),
            )
        } else if self.screen == Screen::Admin {
            Modal {
                opaque: true,
                ..modal(
                    admin_dialog_view(&self.admin.as_view_state()),
                    880.0,
                    editor_height(740.0),
                    Message::CloseAdminDialog,
                )
            }
        } else if self.screen == Screen::Settings {
            modal(
                scrollable(self.settings_dialog_view()).into(),
                440.0,
                editor_height(600.0),
                Message::CloseSettingsDialog,
            )
        } else {
            return None;
        };
        Some(modal)
    }

    pub(super) fn shell_view<'a>(&'a self, size: Size) -> Element<'a, Message> {
        let compact = size.width < 800.0 || size.height < 360.0;
        if compact && self.compact_sidebar_open {
            return self.sidebar_view(true, size.height < 400.0);
        }
        let sidebar_visible = !compact && self.sidebar_visible;
        let mut shell_row = row![].width(Length::Fill).height(Length::Fill);

        if sidebar_visible {
            shell_row = shell_row.push(self.sidebar_view(false, false));
        }

        let conv_state = ConversationViewState {
            characters: &self.characters,
            messages: &self.messages,
            markdown: &self.markdown,
            selected_conversation: self.selected_conversation.as_deref(),
            selected_speaker: self.selected_speaker(),
            input: &self.input,
            active_request: self.active_request,
            stream_text: &self.stream_text,
            typing_character: self.typing_character.as_deref(),
            generation_status: self.generation_status,
            context_usage: self
                .selected_conversation
                .as_ref()
                .and_then(|id| self.context_usage.get(id)),
            editing_message_id: self.editing_message_id.as_deref(),
            editing_message_content: &self.editing_message_content,
            hovered_message_id: self.hovered_message_id.as_deref(),
            sidebar_visible,
            available_size: Size::new(
                size.width - if sidebar_visible { 272.0 } else { 0.0 },
                size.height,
            ),
            glass_mode: self.glass_mode,
            message_images: &self.message_images,
            scene_image: self.scene_image.state(self.image_generation),
        };

        let chat_area = conversation_view(&conv_state, move |message| {
            if compact && matches!(message, ConversationMessage::ToggleSidebar) {
                Message::ToggleCompactSidebar
            } else {
                Message::Conversation(message)
            }
        });

        shell_row = shell_row.push(chat_area);

        container(shell_row)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

struct Modal<'a> {
    content: Element<'a, Message>,
    max_width: f32,
    max_height: f32,
    on_close: Message,
    /// Draw on the solid palette background instead of glass.
    opaque: bool,
}
