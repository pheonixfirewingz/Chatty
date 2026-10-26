use chatty_protocol::Character;
use chatty_protocol::conversation::{ChatMessage, Conversation};
use chatty_protocol::response::GenerationStatus;
use iced::widget::{
    Id, Space, button, canvas, column, container, image, mouse_area, row, scrollable, text,
    text_input, tooltip,
};
use iced::{Alignment, Background, Border, Color, Element, Length, Theme, font};

use crate::characters::image_handle;
use crate::ui::{
    COLOR_PRIMARY, MessageIcon, VectorIcon, card, card_style, render_markdown,
    sidebar_toggle_button, user_chat_bubble_style,
};

/// Interaction messages for the conversation view and new-chat dialog.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum ConversationMessage {
    /// User typed into the composer input.
    InputChanged(String),
    /// Submit composer message (Send or Continue).
    SubmitMessage,
    /// Cancel active generation request.
    CancelRequest(u64),
    /// Start editing an existing message.
    StartEditMessage { id: String, content: String },
    /// Editing message content changed.
    EditContentChanged(String),
    /// Save the edited message.
    SaveMessageEdit,
    /// Cancel the message edit.
    CancelMessageEdit,
    /// Delete a message by ID.
    DeleteMessage(String),
    /// Request regeneration of an assistant response.
    RegenerateMessage(String),
    /// Speak a message locally via TTS.
    SpeakMessage {
        text: String,
        author_id: Option<String>,
    },
    /// Switch to a different variant of a message.
    SelectVariant {
        message_id: String,
        variant_id: String,
    },
    /// Open the new-chat dialog.
    OpenNewChatDialog,
    /// Close the new-chat dialog.
    CloseNewChatDialog,
    /// New conversation title changed.
    NewChatTitleChanged(String),
    /// Selected character for a new conversation.
    SelectNewChatCharacter(String),
    /// Create the new chat conversation.
    CreateNewChat,
    /// Toggle sidebar visibility.
    ToggleSidebar,
    /// Clicked a link in markdown.
    OpenLink(String),
    /// Hovering over a chat message bubble.
    HoverMessage(Option<String>),
}

/// Zero-copy snapshot of application state required to render the conversation view.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ConversationViewState<'a> {
    pub characters: &'a [Character],
    pub conversations: &'a [Conversation],
    pub messages: &'a [ChatMessage],
    pub selected_conversation: Option<&'a str>,
    pub selected_speaker: Option<&'a str>,
    pub input: &'a str,
    pub active_request: Option<u64>,
    pub stream_text: &'a str,
    pub typing_character: Option<&'a str>,
    pub generation_status: Option<GenerationStatus>,
    pub editing_message_id: Option<&'a str>,
    pub editing_message_content: &'a str,
    pub hovered_message_id: Option<&'a str>,
    pub sidebar_visible: bool,
    pub available_size: iced::Size,
    pub light_mode: bool,
    pub glass_mode: bool,
}

/// Zero-copy snapshot of application state required to render the new-chat modal dialog.
#[derive(Debug, Clone)]
pub struct NewChatViewState<'a> {
    pub is_open: bool,
    pub characters: &'a [Character],
    pub selected_character_id: Option<&'a str>,
    pub title: &'a str,
    pub light_mode: bool,
    pub glass_mode: bool,
}

/// Returns the effective content to display for a message (selected variant or default).
#[must_use]
pub fn displayed_message_content(message: &ChatMessage) -> &str {
    message
        .selected_variant_id
        .as_deref()
        .and_then(|selected| {
            message
                .variants
                .iter()
                .find(|variant| variant.id == selected)
        })
        .map(|variant| variant.content.as_str())
        .unwrap_or(&message.content)
}

/// Looks up the display name for a character ID, defaulting to "Assistant".
#[must_use]
pub fn character_name<'a>(characters: &'a [Character], id: Option<&str>) -> &'a str {
    id.and_then(|id| characters.iter().find(|c| c.id == id))
        .map(|c| c.name.as_str())
        .unwrap_or("Assistant")
}

/// Determines the current emotion portrait for the active conversation.
///
/// Looks backwards from the latest message for an `author_id` and `character_image_id`.
/// If none is found, falls back to the selected speaker's default image.
#[must_use]
pub fn current_emotion_portrait(
    characters: &[Character],
    messages: &[ChatMessage],
    selected_conversation: Option<&str>,
    selected_speaker: Option<&str>,
) -> Option<(String, String, String, Vec<u8>)> {
    selected_conversation?;
    for message in messages.iter().rev() {
        let (Some(character_id), Some(image_id)) =
            (message.author_id.as_deref(), message.character_image_id.as_deref())
        else {
            continue;
        };
        let Some(character) = characters.iter().find(|c| c.id == character_id) else {
            continue;
        };
        if let Some(image) = character.images.iter().find(|img| img.id == image_id) {
            return Some((
                character.name.clone(),
                image.label.clone(),
                image.id.clone(),
                image.data.clone(),
            ));
        }
    }

    let character_id = selected_speaker?;
    let character = characters.iter().find(|c| c.id == character_id)?;
    let image_id = character.default_image_id.as_deref()?;
    let image = character.images.iter().find(|img| img.id == image_id)?;
    Some((
        character.name.clone(),
        image.label.clone(),
        image.id.clone(),
        image.data.clone(),
    ))
}

/// Renders a circular character avatar (or initial letter if no image is present).
#[must_use]
pub fn character_avatar_view<'a, Message: 'a>(
    name: &str,
    image_data: Option<&[u8]>,
    size: f32,
) -> Element<'a, Message> {
    if let Some(data) = image_data
        && let Some(handle) = image_handle(data)
    {
        return container(
            image(handle)
                .width(Length::Fixed(size))
                .height(Length::Fixed(size)),
        )
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .style(move |_| container::Style {
            border: Border {
                radius: (size / 2.0).into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            ..Default::default()
        })
        .into();
    }

    let initial = name
        .chars()
        .next()
        .unwrap_or('A')
        .to_ascii_uppercase()
        .to_string();

    container(text(initial).size(size * 0.45).color(Color::WHITE))
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |_| container::Style {
            background: Some(Background::Color(COLOR_PRIMARY)),
            border: Border {
                radius: (size / 2.0).into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            ..Default::default()
        })
        .into()
}

/// Renders the floating emotion portrait card.
#[must_use]
pub fn emotion_portrait_card<'a, Message: 'a>(
    portrait: &(String, String, String, Vec<u8>),
    light_mode: bool,
    glass_mode: bool,
    compact: bool,
) -> Element<'a, Message> {
    let (name, label, _img_id, data) = portrait;
    let img_size = if compact { 80.0 } else { 140.0 };

    let avatar = character_avatar_view(name, Some(data), img_size);
    let title = text(name.clone()).size(13).color(if light_mode {
        Color::from_rgb(0.1, 0.1, 0.2)
    } else {
        Color::WHITE
    });
    let emotion_label = text(format!("Emotion: {label}"))
        .size(11)
        .color(crate::ui::secondary_text_color(light_mode));

    let card_content = column![avatar, Space::new().height(4), title, emotion_label]
        .align_x(Alignment::Center)
        .spacing(2);

    card(card_content, light_mode, glass_mode)
        .width(Length::Fixed(if compact { 110.0 } else { 168.0 }))
        .into()
}

/// Renders an individual chat message bubble (user or assistant).
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn render_message_bubble<'a, Message: Clone + 'a>(
    message: &'a ChatMessage,
    characters: &'a [Character],
    is_editing: bool,
    editing_content: &'a str,
    is_hovered: bool,
    light_mode: bool,
    glass_mode: bool,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    let is_user = message.author_type == "user";

    if is_editing {
        let input_box = text_input("Edit message content…", editing_content)
            .on_input(move |c| on_msg(ConversationMessage::EditContentChanged(c)))
            .padding(10)
            .size(14);

        let save_btn = button(text("Save").size(12))
            .padding([5, 12])
            .on_press(on_msg(ConversationMessage::SaveMessageEdit))
            .style(crate::ui::primary_button_style());

        let cancel_btn = button(text("Cancel").size(12))
            .padding([5, 12])
            .on_press(on_msg(ConversationMessage::CancelMessageEdit))
            .style(crate::ui::secondary_button_style(light_mode));

        let actions = row![save_btn, cancel_btn, text("Esc to cancel").size(11)]
            .spacing(8)
            .align_y(Alignment::Center);

        return container(column![input_box, actions].spacing(8))
            .padding(12)
            .max_width(620)
            .style(move |_| card_style(light_mode, glass_mode))
            .into();
    }

    let displayed_text = displayed_message_content(message);
    let md = render_markdown(displayed_text, move |uri| {
        on_msg(ConversationMessage::OpenLink(uri.to_string()))
    });

    if is_user {
        let bubble = container(md)
            .padding(14)
            .max_width(620)
            .style(move |_| user_chat_bubble_style());

        let actions = {
            let edit_id = &message.id;
            let edit_content = &message.content;
            let edit_btn = message_action_button(
                MessageIcon::Edit,
                "Edit message",
                is_hovered.then(|| {
                    on_msg(ConversationMessage::StartEditMessage {
                        id: edit_id.clone(),
                        content: edit_content.clone(),
                    })
                }),
                light_mode,
            );

            let del_id = message.id.clone();
            let del_btn = message_action_button(
                MessageIcon::Delete,
                "Delete message",
                is_hovered.then(|| on_msg(ConversationMessage::DeleteMessage(del_id))),
                light_mode,
            );

            row![edit_btn, del_btn]
                .spacing(4)
                .align_y(Alignment::Center)
        };

        let msg_id_enter = message.id.clone();
        let interactive = mouse_area(
            column![actions.wrap(), bubble]
                .spacing(4)
                .align_x(Alignment::End),
        )
        .on_enter(on_msg(ConversationMessage::HoverMessage(Some(
            msg_id_enter,
        ))))
        .on_exit(on_msg(ConversationMessage::HoverMessage(None)));

        return row![Space::new().width(Length::Fill), interactive]
            .width(Length::Fill)
            .into();
    }

    let author_name = character_name(characters, message.author_id.as_deref());
    let author_header = text(format!("{author_name}:"))
        .size(13)
        .font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        })
        .color(crate::ui::accent_text_color(light_mode));

    let mut header_row = row![author_header].spacing(8).align_y(Alignment::Center);

    {
        let edit_id = &message.id;
        let edit_content = &message.content;
        let edit_btn = message_action_button(
            MessageIcon::Edit,
            "Edit message",
            is_hovered.then(|| {
                on_msg(ConversationMessage::StartEditMessage {
                    id: edit_id.clone(),
                    content: edit_content.clone(),
                })
            }),
            light_mode,
        );

        let author_id_opt = &message.author_id;
        let speak_btn = message_action_button(
            MessageIcon::Read,
            "Read message aloud",
            is_hovered.then(|| {
                on_msg(ConversationMessage::SpeakMessage {
                    text: displayed_text.to_owned(),
                    author_id: author_id_opt.clone(),
                })
            }),
            light_mode,
        );

        let regen_id = message.id.clone();
        let regen_btn = message_action_button(
            MessageIcon::Regenerate,
            "Regenerate reply",
            is_hovered.then(|| on_msg(ConversationMessage::RegenerateMessage(regen_id))),
            light_mode,
        );

        let del_id = message.id.clone();
        let del_btn = message_action_button(
            MessageIcon::Delete,
            "Delete message",
            is_hovered.then(|| on_msg(ConversationMessage::DeleteMessage(del_id))),
            light_mode,
        );

        let mut actions = row![speak_btn, regen_btn, edit_btn, del_btn].spacing(4);

        if message.variants.len() > 1 {
            let current_variant = message
                .selected_variant_id
                .as_deref()
                .and_then(|sel| message.variants.iter().position(|v| v.id == sel))
                .unwrap_or(0);
            let total = message.variants.len();

            let prev_idx = if current_variant > 0 {
                current_variant - 1
            } else {
                total - 1
            };
            let next_idx = (current_variant + 1) % total;

            let msg_id1 = message.id.clone();
            let prev_v_id = message.variants[prev_idx].id.clone();
            let prev_btn = message_action_button(
                MessageIcon::Previous,
                "Previous variant",
                is_hovered.then(|| {
                    on_msg(ConversationMessage::SelectVariant {
                        message_id: msg_id1,
                        variant_id: prev_v_id,
                    })
                }),
                light_mode,
            );

            let msg_id2 = message.id.clone();
            let next_v_id = message.variants[next_idx].id.clone();
            let next_btn = message_action_button(
                MessageIcon::Next,
                "Next variant",
                is_hovered.then(|| {
                    on_msg(ConversationMessage::SelectVariant {
                        message_id: msg_id2,
                        variant_id: next_v_id,
                    })
                }),
                light_mode,
            );

            let counter = text(format!("{}/{}", current_variant + 1, total))
                .size(11)
                .color(if is_hovered {
                    crate::ui::palette(light_mode).text
                } else {
                    Color::TRANSPARENT
                });

            let variant_controls = row![prev_btn, counter, next_btn]
                .spacing(4)
                .align_y(Alignment::Center);

            actions = actions.push(Space::new().width(8)).push(variant_controls);
        }

        header_row = header_row.push(actions.wrap());
    }

    let assistant_content = column![header_row.wrap(), Space::new().height(2), md].spacing(2);

    let msg_id_enter = message.id.clone();
    let area = mouse_area(
        container(assistant_content)
            .padding(10)
            .max_width(700)
            .width(Length::Fill)
            .style(move |_| crate::ui::assistant_chat_bubble_style(light_mode, glass_mode)),
    )
    .on_enter(on_msg(ConversationMessage::HoverMessage(Some(
        msg_id_enter,
    ))))
    .on_exit(on_msg(ConversationMessage::HoverMessage(None)));

    row![area].width(Length::Fill).into()
}

fn message_action_button<'a, Message: Clone + 'a>(
    icon: MessageIcon,
    label: &'static str,
    action: Option<Message>,
    light_mode: bool,
) -> Element<'a, Message> {
    let visible = action.is_some();
    let color = if visible {
        crate::ui::palette(light_mode).text
    } else {
        Color::TRANSPARENT
    };
    let icon = canvas::Canvas::new(VectorIcon::new(icon).with_color(color))
        .width(24)
        .height(24);
    let control = button(icon)
        .width(28)
        .height(28)
        .padding(2)
        .on_press_maybe(action)
        .style(message_action_style(light_mode, visible));
    tooltip(
        control,
        container(text(label).size(12))
            .padding(6)
            .style(crate::ui::tooltip_style),
        tooltip::Position::Top,
    )
    .into()
}

// Hidden action buttons retain their measured size so hover never reflows text.
fn message_action_style(
    light_mode: bool,
    visible: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        if visible {
            crate::ui::chat_action_button_style(light_mode, false)(theme, status)
        } else {
            button::Style {
                background: None,
                text_color: Color::TRANSPARENT,
                border: Border::default(),
                shadow: iced::Shadow::default(),
                snap: false,
            }
        }
    }
}

/// Primary conversation view combining chat history, streaming responses, and composer.
#[must_use]
pub fn conversation_view<'a, Message: Clone + 'a>(
    view: &ConversationViewState<'a>,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    let light_mode = view.light_mode;
    let glass_mode = view.glass_mode;
    if view.selected_conversation.is_none() {
        let heading = text("Who will you meet today?")
            .size(26)
            .color(crate::ui::accent_text_color(light_mode));
        let subtitle = text("Pick a character, set the scene, and start a conversation.")
            .size(14)
            .color(crate::ui::secondary_text_color(light_mode));

        let start_btn = button(text("Start a new chat").size(15).font(font::Font {
                weight: font::Weight::Bold,
                ..Default::default()
            }))
            .padding([10, 24])
            .on_press(on_msg(ConversationMessage::OpenNewChatDialog))
            .style(crate::ui::primary_button_style());

        let center_col = card(
            column![heading, subtitle, Space::new().height(12), start_btn]
                .align_x(Alignment::Center)
                .spacing(8),
            light_mode,
            glass_mode,
        )
        .max_width(480);

        let content: Element<'a, Message> = if !view.sidebar_visible {
            let toggle_btn = sidebar_toggle_button(false, on_msg(ConversationMessage::ToggleSidebar));
            column![
                row![toggle_btn].align_y(Alignment::Center),
                container(center_col)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        } else {
            container(center_col)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        };

        return container(content)
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    }

    let emotion_portrait = if view.available_size.width >= 900.0
        && view.available_size.height >= 500.0
    {
        current_emotion_portrait(
            view.characters,
            view.messages,
            view.selected_conversation,
            view.selected_speaker,
        )
    } else {
        None
    };

    let mut message_list = column![].spacing(16).width(Length::Fill);

    for msg in view.messages {
        let is_editing = view.editing_message_id == Some(msg.id.as_str());
        let is_hovered = view.hovered_message_id == Some(msg.id.as_str());
        message_list = message_list.push(render_message_bubble(
            msg,
            view.characters,
            is_editing,
            view.editing_message_content,
            is_hovered,
            view.light_mode,
            view.glass_mode,
            on_msg,
        ));
    }

    if !view.stream_text.is_empty() {
        let author_name = character_name(view.characters, view.typing_character);
        let header = text(format!("{author_name}:"))
            .size(13)
            .color(crate::ui::accent_text_color(light_mode));
        let md = render_markdown(view.stream_text, move |uri| {
            on_msg(ConversationMessage::OpenLink(uri.to_string()))
        });

        let stream_content = container(column![header, Space::new().height(2), md].spacing(2))
            .padding(10)
            .max_width(700)
            .style(move |_| crate::ui::assistant_chat_bubble_style(light_mode, glass_mode));

        message_list = message_list.push(row![stream_content]);
    }

    if let Some(speaker_id) = view.typing_character {
        let typing_name = character_name(view.characters, Some(speaker_id));
        let status_text = match view.generation_status {
            Some(GenerationStatus::SelectingEmotion) => {
                "AI is selecting an emotion portrait…".to_string()
            }
            None => format!("{typing_name} is typing…"),
        };
        message_list = message_list.push(
            container(
                text(status_text)
                    .size(12)
                    .color(crate::ui::secondary_text_color(light_mode)),
            )
                .padding(8)
                .style(move |_| crate::ui::assistant_chat_bubble_style(light_mode, glass_mode)),
        );
    }

    let scrollable_messages = scrollable(
        container(message_list)
            .padding(if view.available_size.width < 500.0 {
                8
            } else {
                20
            })
            .width(Length::Fill),
    )
    .id(Id::new("chat-messages"))
    .height(Length::Fill)
    .width(Length::Fill);

    let composer_input = text_input("Message your character…", view.input)
        .style(crate::ui::input_style)
        .on_input(move |v| on_msg(ConversationMessage::InputChanged(v)))
        .on_submit(on_msg(ConversationMessage::SubmitMessage))
        .padding(12)
        .size(14)
        .width(Length::Fill);

    let action_btn = if let Some(req_id) = view.active_request {
        button(text("Stop").size(13).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        }))
        .padding([10, 18])
        .on_press(on_msg(ConversationMessage::CancelRequest(req_id)))
        .style(crate::ui::danger_button_style())
    } else {
        let label = if view.input.trim().is_empty() {
            "Continue"
        } else {
            "Send"
        };
        button(text(label).size(13).font(font::Font {
            weight: font::Weight::Bold,
            ..Default::default()
        }))
        .padding([10, 18])
        .on_press(on_msg(ConversationMessage::SubmitMessage))
        .style(crate::ui::primary_button_style())
    };

    let hint = text("Enter to send")
        .size(12)
        .color(crate::ui::secondary_text_color(light_mode));

    let composer_box = card(
        column![
            row![composer_input, action_btn]
                .spacing(8)
                .align_y(Alignment::Center),
            hint
        ]
        .spacing(4),
        light_mode,
        glass_mode,
    )
    .width(Length::Fill);

    let mut chat_content = column![].spacing(8).width(Length::Fill).height(Length::Fill);

    if !view.sidebar_visible {
        let toggle_btn = sidebar_toggle_button(false, on_msg(ConversationMessage::ToggleSidebar));
        chat_content = chat_content.push(row![toggle_btn].align_y(Alignment::Center));
    }

    chat_content = chat_content.push(scrollable_messages).push(composer_box);

    let chat_bg = crate::ui::chat_panel_background(light_mode, glass_mode);
    if let Some(ref portrait) = emotion_portrait {
        let emotion_card = emotion_portrait_card(portrait, light_mode, glass_mode, false);
        let content_with_portrait = row![chat_content, Space::new().width(12), emotion_card]
            .width(Length::Fill)
            .height(Length::Fill);
        container(content_with_portrait)
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(chat_bg),
                ..Default::default()
            })
            .into()
    } else {
        container(chat_content)
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(chat_bg),
                ..Default::default()
            })
            .into()
    }
}

/// Modal dialog for creating a new chat conversation.
#[must_use]
pub fn new_chat_dialog_view<'a, Message: Clone + 'a>(
    view: &NewChatViewState<'a>,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Option<Element<'a, Message>> {
    if !view.is_open {
        return None;
    }
    let light_mode = view.light_mode;
    let _glass_mode = view.glass_mode;

    let title_text = text("New chat")
        .size(20)
        .color(crate::ui::accent_text_color(light_mode));
    let desc_text = text("Choose a character for a private chat.")
        .size(13)
        .color(crate::ui::secondary_text_color(light_mode));

    let name_label = text("CONVERSATION NAME").size(11);
    let name_input = text_input("Optional — we can name it for you", view.title)
        .on_input(move |t| on_msg(ConversationMessage::NewChatTitleChanged(t)))
        .padding(8)
        .size(13);

    let char_label = text("CHARACTERS").size(11);

    let mut char_list = column![].spacing(8);

    for c in view.characters {
        let is_selected = view.selected_character_id == Some(c.id.as_str());

        let avatar = character_avatar_view(
            &c.name,
            c.images.first().map(|img| img.data.as_slice()),
            36.0,
        );

        let name_t = text(&c.name).size(14);
        let summary_t = text(if c.personality.trim().is_empty() {
            "Ready to chat"
        } else {
            &c.personality
        })
        .size(11)
        .color(crate::ui::secondary_text_color(light_mode));

        let char_info = column![name_t, summary_t].spacing(2);

        let radio_marker =
            text(if is_selected { "●" } else { "○" })
                .size(16)
                .color(if is_selected {
                    COLOR_PRIMARY
                } else {
                    crate::ui::secondary_text_color(light_mode)
                });

        let card_row = row![
            avatar,
            char_info,
            Space::new().width(Length::Fill),
            radio_marker
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        let char_id = c.id.clone();
        let card_btn = button(card_row)
            .width(Length::Fill)
            .padding(10)
            .on_press(on_msg(ConversationMessage::SelectNewChatCharacter(
                char_id,
            )))
            .style(move |theme: &Theme, status: button::Status| {
                if is_selected {
                    button::Style {
                        background: Some(Background::Color(crate::ui::COLOR_PRIMARY_STRONG)),
                        text_color: Color::WHITE,
                        border: Border {
                            radius: 8.0.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                } else {
                    let (bg, border_color) = match status {
                        button::Status::Hovered => (
                            Color::from_rgba(theme.palette().text.r, theme.palette().text.g, theme.palette().text.b, 0.08),
                            Color::from_rgba(theme.palette().text.r, theme.palette().text.g, theme.palette().text.b, 0.16),
                        ),
                        _ => (
                            Color::from_rgba(theme.palette().text.r, theme.palette().text.g, theme.palette().text.b, 0.04),
                            Color::from_rgba(theme.palette().text.r, theme.palette().text.g, theme.palette().text.b, 0.08),
                        ),
                    };
                    button::Style {
                        background: Some(Background::Color(bg)),
                        text_color: theme.palette().text,
                        border: Border {
                            color: border_color,
                            width: 0.0,
                            radius: 8.0.into(),
                        },
                        ..Default::default()
                    }
                }
            });

        char_list = char_list.push(card_btn);
    }

    let scrollable_chars = scrollable(char_list).height(Length::Fill);

    let create_enabled = view.selected_character_id.is_some();
    let mut create_btn = button(text("Create conversation").size(14).font(font::Font {
        weight: font::Weight::Bold,
        ..Default::default()
    }))
    .padding([10, 20])
    .width(Length::Fill)
    .style(crate::ui::primary_button_style());

    if create_enabled {
        create_btn = create_btn.on_press(on_msg(ConversationMessage::CreateNewChat));
    }

    let cancel_btn = button(text("Cancel").size(13))
        .padding([8, 16])
        .on_press(on_msg(ConversationMessage::CloseNewChatDialog))
        .style(crate::ui::secondary_button_style(light_mode));

    let dialog_content = column![
        title_text,
        desc_text,
        Space::new().height(10),
        name_label,
        name_input,
        Space::new().height(10),
        char_label,
        scrollable_chars,
        Space::new().height(14),
        create_btn,
        cancel_btn,
    ]
    .spacing(6)
    .align_x(Alignment::Center);

    Some(
        container(dialog_content)
            .padding(20)
            .width(Length::Fill)
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_protocol::conversation::Variant;
    use chatty_protocol::CharacterImage;

    fn dummy_character(id: &str, name: &str) -> Character {
        Character {
            id: id.into(),
            name: name.into(),
            description: "A test bot".into(),
            personality: "Friendly companion".into(),
            scenario: String::new(),
            system_prompt: String::new(),
            example_dialogue: String::new(),
            appearance: String::new(),
            age: String::new(),
            gender: String::new(),
            race: String::new(),
            misc: String::new(),
            tags: Vec::new(),
            avatar: None,
            images: Vec::new(),
            default_image_id: None,
            is_public: false,
            owned_by_user: true,
            revision: 1,
        }
    }

    fn dummy_message(id: &str, author_type: &str, content: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            author_type: author_type.into(),
            author_id: if author_type == "user" {
                None
            } else {
                Some("bot1".into())
            },
            content: content.into(),
            parent_id: None,
            selected_variant_id: None,
            character_image_id: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            revision: 1,
            variants: Vec::new(),
        }
    }

    #[test]
    fn test_displayed_message_content_default() {
        let msg = dummy_message("m1", "user", "Hello world");
        assert_eq!(displayed_message_content(&msg), "Hello world");
    }

    #[test]
    fn test_displayed_message_content_variant() {
        let mut msg = dummy_message("m1", "assistant", "Default response");
        msg.variants = vec![
            Variant {
                id: "v1".into(),
                content: "Alternative 1".into(),
                created_at: "2026-01-01".into(),
                revision: 1,
            },
            Variant {
                id: "v2".into(),
                content: "Alternative 2".into(),
                created_at: "2026-01-01".into(),
                revision: 1,
            },
        ];
        msg.selected_variant_id = Some("v2".into());
        assert_eq!(displayed_message_content(&msg), "Alternative 2");
    }

    #[test]
    fn test_character_name() {
        let chars = vec![dummy_character("bot1", "Seraphina")];
        assert_eq!(character_name(&chars, Some("bot1")), "Seraphina");
        assert_eq!(character_name(&chars, Some("nonexistent")), "Assistant");
        assert_eq!(character_name(&chars, None), "Assistant");
    }

    #[test]
    fn test_current_emotion_portrait_fallback() {
        let mut character = dummy_character("bot1", "Seraphina");
        character.default_image_id = Some("img1".into());
        character.images.push(CharacterImage {
            id: "img1".into(),
            label: "neutral".into(),
            data: vec![1, 2, 3],
        });
        let chars = vec![character];
        let messages = vec![dummy_message("m1", "assistant", "Hi")];

        let portrait =
            current_emotion_portrait(&chars, &messages, Some("c1"), Some("bot1"));
        assert!(portrait.is_some());
        let (name, label, id, data) = portrait.unwrap();
        assert_eq!(name, "Seraphina");
        assert_eq!(label, "neutral");
        assert_eq!(id, "img1");
        assert_eq!(data, vec![1, 2, 3]);
    }

    #[test]
    fn test_conversation_view_empty_state() {
        let view_state = ConversationViewState {
            characters: &[],
            conversations: &[],
            messages: &[],
            selected_conversation: None,
            selected_speaker: None,
            input: "",
            active_request: None,
            stream_text: "",
            typing_character: None,
            generation_status: None,
            editing_message_id: None,
            editing_message_content: "",
            hovered_message_id: None,
            sidebar_visible: true,
            available_size: iced::Size::new(800.0, 720.0),
            light_mode: false,
            glass_mode: false,
        };

        let _el: Element<'_, ConversationMessage> =
            conversation_view(&view_state, std::convert::identity);
    }

    #[test]
    fn test_new_chat_dialog_rendering() {
        let chars = vec![dummy_character("c1", "Alice")];
        let view_state = NewChatViewState {
            is_open: true,
            characters: &chars,
            selected_character_id: Some("c1"),
            title: "My Chat",
            light_mode: false,
            glass_mode: false,
        };

        let dialog = new_chat_dialog_view(&view_state, std::convert::identity);
        assert!(dialog.is_some());
    }

    #[test]
    fn test_conversation_view_collapsed_sidebar() {
        let view_state = ConversationViewState {
            characters: &[],
            conversations: &[],
            messages: &[],
            selected_conversation: None,
            selected_speaker: None,
            input: "",
            active_request: None,
            stream_text: "",
            typing_character: None,
            generation_status: None,
            editing_message_id: None,
            editing_message_content: "",
            hovered_message_id: None,
            sidebar_visible: false,
            available_size: iced::Size::new(800.0, 720.0),
            light_mode: false,
            glass_mode: false,
        };

        let _el: Element<'_, ConversationMessage> =
            conversation_view(&view_state, std::convert::identity);
    }

    #[test]
    fn test_conversation_view_with_messages_hover() {
        let chars = vec![dummy_character("bot1", "Mara")];
        let msgs = vec![
            dummy_message("m1", "user", "Tell me about this place."),
            dummy_message("m2", "assistant", "It has watched the valley for centuries."),
        ];
        let conv = Conversation {
            id: "c1".into(),
            title: "Chat 1".into(),
            participant_ids: vec!["bot1".into()],
            kind: chatty_protocol::conversation::ConversationKind::Direct,
            state: String::new(),
            summary: String::new(),
            revision: 1,
        };
        let convs = vec![conv];

        let view_state_unhovered = ConversationViewState {
            characters: &chars,
            conversations: &convs,
            messages: &msgs,
            selected_conversation: Some("c1"),
            selected_speaker: Some("bot1"),
            input: "",
            active_request: None,
            stream_text: "",
            typing_character: None,
            generation_status: None,
            editing_message_id: None,
            editing_message_content: "",
            hovered_message_id: None,
            sidebar_visible: false,
            available_size: iced::Size::new(800.0, 720.0),
            light_mode: false,
            glass_mode: false,
        };
        let _el1: Element<'_, ConversationMessage> =
            conversation_view(&view_state_unhovered, std::convert::identity);

        let view_state_user_hover = ConversationViewState {
            hovered_message_id: Some("m1"),
            ..view_state_unhovered
        };
        let _el2: Element<'_, ConversationMessage> =
            conversation_view(&view_state_user_hover, std::convert::identity);

        let view_state_asst_hover = ConversationViewState {
            hovered_message_id: Some("m2"),
            ..view_state_unhovered
        };
        let _el3: Element<'_, ConversationMessage> =
            conversation_view(&view_state_asst_hover, std::convert::identity);
    }
}
