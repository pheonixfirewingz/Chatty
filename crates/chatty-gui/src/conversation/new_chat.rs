//! New-chat dialog: a fuzzy-searchable gallery of character profile cards.

use super::*;
use crate::characters::{image_handle, portrait_bytes, search_characters};
use crate::ui::{
    ACCENT_TEXT, BOLD, COLOR_PRIMARY, MessageIcon, SECONDARY_TEXT, SidebarToggle, Tone, action_icon,
};
use chatty_protocol::Character;
use iced::widget::{
    button, column, container, image, responsive, row, scrollable, stack, text, text_input, tooltip,
};
use iced::{
    Alignment, Background, Border, Color, ContentFit, Degrees, Element, Length, Padding, Shadow,
    Theme, Vector, gradient,
};

/// Narrowest card before the gallery drops a column.
const CARD_MIN_WIDTH: f32 = 180.0;
const CARD_GAP: f32 = 12.0;
/// Portrait height relative to card width: a 4:5 profile photo.
const PORTRAIT_RATIO: f32 = 1.25;
/// Tag chips sit in a fixed band so cards in a row align.
const CARD_BODY_HEIGHT: f32 = 36.0;
const CARD_RADIUS: f32 = 14.0;
/// Selection outline; unselected cards keep the inset so content does not move.
const CARD_OUTLINE: f32 = 2.0;
/// Estimated chip width per character and for padding at the 11 px tag size;
/// slightly generous so a chip never ends up cut by the card edge.
const TAG_CHAR_WIDTH: f32 = 6.2;
const TAG_CHIP_PADDING: f32 = 16.0;
const TAG_SPACING: f32 = 4.0;
/// Right padding that keeps the scrollbar off the cards.
const SCROLLBAR_GUTTER: f32 = 12.0;

/// Modal dialog for creating a chat, or editing the open chat's title,
/// characters, and group speaker mode.
#[must_use]
pub fn new_chat_dialog_view<'a, Message: Clone + 'a>(
    view: &NewChatViewState<'a>,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Option<Element<'a, Message>> {
    if !view.is_open {
        return None;
    }
    let view = view.clone();
    let matches = search_characters(view.characters, view.search);
    Some(
        responsive(move |size| dialog_at_width(&view, &matches, size.width, on_msg))
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
    )
}

fn dialog_at_width<'a, Message: Clone + 'a>(
    view: &NewChatViewState<'a>,
    matches: &[&'a Character],
    width: f32,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    const PADDING: f32 = 16.0;
    let selected = view.selected_character_ids;
    let group = selected.len() > 1;

    let action_label = match (selected.is_empty(), view.editing, group) {
        (true, ..) => "Pick at least one character first",
        (false, true, _) => "Save chat",
        (false, false, true) => "Create group chat",
        (false, false, false) => "Create chat",
    };
    let header = row![
        text(if view.editing {
            "Edit chat"
        } else {
            "New chat"
        })
        .size(22)
        .font(BOLD)
        .width(Length::Fill),
        action_icon(
            if view.editing {
                MessageIcon::Save
            } else {
                MessageIcon::NewChat
            },
            action_label,
            (!selected.is_empty()).then(|| on_msg(ConversationMessage::CreateNewChat)),
            Tone::Primary,
        ),
        crate::ui::ghost_icon_button(
            SidebarToggle::Collapse,
            Some("Close"),
            on_msg(ConversationMessage::CloseNewChatDialog),
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let search = text_input("Search characters…", view.search)
        .on_input(move |query| on_msg(ConversationMessage::NewChatSearchChanged(query)))
        .size(14)
        .padding([8, 10])
        .style(crate::ui::input_style)
        .width(Length::Fill);

    let gallery = gallery_view(view, matches, width - 2.0 * PADDING, on_msg);
    let setup = setup_row(view, width - 2.0 * PADDING, on_msg);

    container(
        column![header, search, gallery, setup]
            .spacing(12)
            .height(Length::Fill),
    )
    .padding(PADDING)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn gallery_view<'a, Message: Clone + 'a>(
    view: &NewChatViewState<'a>,
    matches: &[&'a Character],
    width: f32,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    if matches.is_empty() {
        let message = if view.characters.is_empty() {
            "No characters yet. Create one in the Characters dialog first."
        } else {
            "No characters match your search."
        };
        return container(text(message).size(13).color(SECONDARY_TEXT))
            .center(Length::Fill)
            .into();
    }

    let usable = (width - SCROLLBAR_GUTTER).max(CARD_MIN_WIDTH);
    let columns = (((usable + CARD_GAP) / (CARD_MIN_WIDTH + CARD_GAP)).floor() as usize).max(1);
    let card_width = ((usable - CARD_GAP * (columns - 1) as f32) / columns as f32).floor();

    let mut grid = column![].spacing(CARD_GAP);
    for chunk in matches.chunks(columns) {
        let mut line = row![].spacing(CARD_GAP);
        for character in chunk {
            line = line.push(profile_card(view, character, card_width, on_msg));
        }
        grid = grid.push(line);
    }

    scrollable(container(grid).padding(Padding {
        right: SCROLLBAR_GUTTER,
        ..Padding::ZERO
    }))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// A dating-app style profile card: a tall portrait with the name over a
/// shaded band, then a line of tags.
fn profile_card<'a, Message: Clone + 'a>(
    view: &NewChatViewState<'a>,
    character: &'a Character,
    width: f32,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    let selected = view.selected_character_ids;
    let position = selected.iter().position(|id| *id == character.id);
    let is_selected = position.is_some();
    let full = selected.len() >= MAX_CHAT_CHARACTERS;
    let inner_width = width - 2.0 * CARD_OUTLINE;
    let photo_height = (inner_width * PORTRAIT_RATIO).round();
    let top_radius = iced::border::top(CARD_RADIUS - CARD_OUTLINE);

    let photo: Element<'a, Message> = match portrait_bytes(character).and_then(image_handle) {
        Some(handle) => image(handle)
            .width(Length::Fill)
            .height(photo_height)
            .content_fit(ContentFit::Cover)
            .border_radius(top_radius)
            .into(),
        None => {
            let initial = character
                .name
                .chars()
                .next()
                .unwrap_or('?')
                .to_uppercase()
                .to_string();
            container(
                text(initial)
                    .size(inner_width * 0.32)
                    .font(BOLD)
                    .color(Color::WHITE),
            )
            .center_x(Length::Fill)
            .center_y(photo_height)
            .style(move |_| container::Style {
                background: Some(Background::Gradient(
                    gradient::Linear::new(Degrees(135.0))
                        .add_stop(0.0, crate::ui::COLOR_PRIMARY_STRONG)
                        .add_stop(1.0, Color::from_rgb(0.08, 0.10, 0.18))
                        .into(),
                )),
                border: Border {
                    radius: top_radius,
                    ..Border::default()
                },
                ..container::Style::default()
            })
            .into()
        }
    };

    let mut details = row![].spacing(6).align_y(Alignment::Center);
    let soft_white = Color::from_rgba(1.0, 1.0, 1.0, 0.8);
    let mut first = true;
    for field in [&character.race, &character.gender] {
        let field = field.trim();
        if !field.is_empty() {
            if !first {
                details = details.push(text("·").size(12).color(soft_white));
            }
            details = details.push(text(field).size(12).color(soft_white));
            first = false;
        }
    }
    let identity = column![
        row![
            text(&character.name)
                .size(17)
                .font(BOLD)
                .color(Color::WHITE),
            text(character.age.trim()).size(15).color(Color::WHITE),
        ]
        .spacing(6)
        .align_y(Alignment::End),
        details,
    ]
    .spacing(2);
    let view_glass = view.glass_mode;
    let shade = container(identity)
        .padding(Padding {
            top: 28.0,
            right: 10.0,
            bottom: 8.0,
            left: 10.0,
        })
        .width(Length::Fill)
        .style(move |_| {
            // Fade into the card body so the photo has no hard bottom edge.
            let plate = crate::ui::surface_raised_color(view_glass);
            let with_alpha = |a| Color { a, ..plate };
            container::Style {
                background: Some(Background::Gradient(
                    gradient::Linear::new(Degrees(180.0))
                        .add_stop(0.0, with_alpha(0.0))
                        .add_stop(0.55, with_alpha(0.7))
                        .add_stop(1.0, with_alpha(plate.a))
                        .into(),
                )),
                ..container::Style::default()
            }
        });

    let mut layers = stack![
        photo,
        container(shade)
            .align_bottom(photo_height)
            .width(Length::Fill),
    ]
    .width(Length::Fill)
    .height(photo_height);
    if let Some(index) = position {
        // A group shows its speaking order; a single pick shows a check mark.
        let label = if selected.len() > 1 {
            (index + 1).to_string()
        } else {
            "✓".to_owned()
        };
        let badge = container(text(label).size(14).font(BOLD).color(Color::WHITE))
            .center(28)
            .style(|_| container::Style {
                background: Some(Background::Color(COLOR_PRIMARY)),
                border: Border {
                    radius: 14.0.into(),
                    width: 2.0,
                    color: Color::WHITE,
                },
                ..container::Style::default()
            });
        layers = layers.push(
            container(badge)
                .align_right(Length::Fill)
                .padding(8)
                .width(Length::Fill),
        );
    }

    let tags = tag_row(&character.tags, inner_width - 20.0);
    let body = container(tags)
        .padding([8, 10])
        .width(Length::Fill)
        .height(CARD_BODY_HEIGHT)
        .clip(true);

    let glass_mode = view.glass_mode;
    let same_world = is_selected || can_join_group(view.worlds, selected, &character.id);
    let disabled = !is_selected && (full || !same_world);
    if disabled {
        layers = layers.push(
            container(iced::widget::Space::new())
                .width(Length::Fill)
                .height(photo_height)
                .style(move |_| container::Style {
                    background: Some(Background::Color(Color::from_rgba(0.05, 0.06, 0.09, 0.58))),
                    border: Border {
                        radius: top_radius,
                        ..Border::default()
                    },
                    ..container::Style::default()
                }),
        );
    }
    let card = button(column![layers, body])
        .width(width)
        .padding(CARD_OUTLINE)
        .on_press_maybe((!disabled).then(|| {
            on_msg(ConversationMessage::ToggleNewChatCharacter(
                character.id.clone(),
            ))
        }))
        .style(move |theme: &Theme, status| {
            profile_card_style(theme, status, is_selected, glass_mode)
        });

    if disabled {
        let reason = if !same_world {
            "Characters in a group must belong to the same world".to_owned()
        } else {
            format!("A chat holds up to {MAX_CHAT_CHARACTERS} characters")
        };
        tooltip(
            card,
            container(text(reason).size(12))
                .padding([4, 8])
                .style(crate::ui::tooltip_style),
            tooltip::Position::Top,
        )
        .into()
    } else {
        card.into()
    }
}

fn tag_chip_width(label: &str) -> f32 {
    TAG_CHIP_PADDING + TAG_CHAR_WIDTH * label.chars().count() as f32
}

/// Whole tag chips that fit `width`, then a "+N" chip for the rest.
fn tag_row<'a, Message: 'a>(tags: &'a [String], width: f32) -> Element<'a, Message> {
    let mut chips = row![].spacing(TAG_SPACING);
    let mut used = 0.0;
    for (index, tag) in tags.iter().enumerate() {
        let rest = tags.len() - index - 1;
        // Keep room for the "+N" chip unless this is the last tag.
        let reserve = if rest > 0 {
            let digits = rest.to_string().len() as f32;
            TAG_SPACING + TAG_CHIP_PADDING + TAG_CHAR_WIDTH * (1.0 + digits)
        } else {
            0.0
        };
        let needed = tag_chip_width(tag) + if index > 0 { TAG_SPACING } else { 0.0 };
        if used + needed + reserve > width {
            chips = chips.push(tag_chip(format!("+{}", tags.len() - index)));
            break;
        }
        used += needed;
        chips = chips.push(tag_chip(tag.as_str()));
    }
    chips.into()
}

fn tag_chip<'a, Message: 'a>(tag: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    container(text(tag).size(11).color(ACCENT_TEXT))
        .padding([2, 8])
        .style(|_| container::Style {
            background: Some(Background::Color(Color::from_rgba(
                COLOR_PRIMARY.r,
                COLOR_PRIMARY.g,
                COLOR_PRIMARY.b,
                0.18,
            ))),
            border: Border {
                radius: 10.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

fn profile_card_style(
    theme: &Theme,
    status: button::Status,
    selected: bool,
    glass_mode: bool,
) -> button::Style {
    let primary = theme.palette().primary;
    let outline = match (selected, status) {
        (true, _) => COLOR_PRIMARY,
        (false, button::Status::Hovered | button::Status::Pressed) => {
            Color::from_rgba(primary.r, primary.g, primary.b, 0.6)
        }
        _ => Color::TRANSPARENT,
    };
    let disabled = matches!(status, button::Status::Disabled);
    let lift = (matches!(status, button::Status::Hovered) && !disabled) as u8 as f32;
    button::Style {
        background: Some(crate::ui::raised_background(glass_mode)),
        text_color: if disabled {
            SECONDARY_TEXT
        } else {
            theme.palette().text
        },
        border: Border {
            color: outline,
            width: CARD_OUTLINE,
            radius: CARD_RADIUS.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if disabled { 0.2 } else { 0.42 }),
            offset: Vector::new(0.0, 6.0 + 2.0 * lift),
            blur_radius: 18.0 + 6.0 * lift,
        },
        snap: false,
    }
}

/// Chat name and group speaker mode under the gallery.
fn setup_row<'a, Message: Clone + 'a>(
    view: &NewChatViewState<'a>,
    width: f32,
    on_msg: impl Fn(ConversationMessage) -> Message + Copy + 'a,
) -> Element<'a, Message> {
    let group = view.selected_character_ids.len() > 1;
    let caption = |label| text(label).size(12).color(SECONDARY_TEXT);
    let name_field = column![
        caption("Chat name"),
        text_input("Optional, e.g. Tavern night", view.title)
            .on_input(move |title| on_msg(ConversationMessage::NewChatTitleChanged(title)))
            .size(14)
            .padding([8, 10])
            .style(crate::ui::input_style),
    ]
    .spacing(4)
    .width(Length::Fill);

    // The mode picker stays in place for one character so the layout does not jump.
    let mut modes = row![].spacing(6);
    for (kind, label, help) in GROUP_MODES {
        let active = group && view.kind == kind;
        let mode = button(text(label).size(13))
            .padding([6, 12])
            .on_press_maybe(group.then(|| on_msg(ConversationMessage::NewChatModeChanged(kind))))
            .style(move |theme: &Theme, status| {
                if active {
                    crate::ui::primary_button_style()(theme, status)
                } else {
                    crate::ui::secondary_button_style()(theme, status)
                }
            });
        let help = if group {
            help
        } else {
            "Pick two or more characters to make a group"
        };
        modes = modes.push(tooltip(
            mode,
            container(text(help).size(12))
                .padding([4, 8])
                .style(crate::ui::tooltip_style),
            tooltip::Position::Top,
        ));
    }
    let mode_field = column![caption("Who replies"), modes.wrap()]
        .spacing(4)
        .width(Length::Fill);

    if width < 640.0 {
        column![name_field, mode_field].spacing(10).into()
    } else {
        row![name_field, mode_field].spacing(12).into()
    }
}
