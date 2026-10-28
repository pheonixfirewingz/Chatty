//! Character roster and editor views.

use super::*;
use chatty_protocol::{Character, World};
use iced::widget::{
    Space, button, column, container, image, row, scrollable, text, text_editor, text_input,
    toggler,
};
use iced::{Alignment, Background, Color, Element, Length, Padding, Theme};
use std::collections::HashSet;

fn roster_button_style(
    theme: &Theme,
    status: button::Status,
    selected: bool,
    glass_mode: bool,
) -> button::Style {
    let palette = theme.palette();
    if selected {
        button::Style {
            background: Some(Background::Color(Color::from_rgba(
                crate::ui::COLOR_PRIMARY.r,
                crate::ui::COLOR_PRIMARY.g,
                crate::ui::COLOR_PRIMARY.b,
                0.22,
            ))),
            text_color: palette.text,
            border: iced::border::Border {
                color: crate::ui::COLOR_PRIMARY,
                width: 1.5,
                radius: 8.0.into(),
            },
            shadow: iced::Shadow::default(),
            snap: false,
        }
    } else {
        let (bg, border_color) = match status {
            button::Status::Active => (
                crate::ui::raised_background(glass_mode),
                crate::ui::border_color(glass_mode),
            ),
            button::Status::Hovered => (
                Background::Color(Color::from_rgba(
                    palette.primary.r,
                    palette.primary.g,
                    palette.primary.b,
                    0.12,
                )),
                crate::ui::COLOR_PRIMARY,
            ),
            button::Status::Pressed => (
                Background::Color(Color::from_rgba(
                    palette.primary.r,
                    palette.primary.g,
                    palette.primary.b,
                    0.20,
                )),
                crate::ui::COLOR_PRIMARY,
            ),
            button::Status::Disabled => {
                (crate::ui::raised_background(glass_mode), Color::TRANSPARENT)
            }
        };
        button::Style {
            background: Some(bg),
            text_color: palette.text,
            border: iced::border::Border {
                color: border_color,
                width: 0.0,
                radius: 8.0.into(),
            },
            shadow: iced::Shadow::default(),
            snap: false,
        }
    }
}

/// Renders the scrollable roster of characters with a search filter and avatar portraits.
#[must_use]
pub fn character_list_view<'a>(
    characters: &'a [Character],
    selected_id: Option<&'a str>,
    search_query: &'a str,
    char_import_pending: bool,
    glass_mode: bool,
) -> Element<'a, CharacterMessage> {
    use crate::ui::{MessageIcon, Tone, action_icon};
    let new_btn = action_icon(
        MessageIcon::Add,
        "New character",
        Some(CharacterMessage::New),
        Tone::Primary,
    );
    let import_btn = action_icon(
        MessageIcon::Import,
        if char_import_pending {
            "Importing…"
        } else {
            "Import character (.zip, .json, or .png card)"
        },
        (!char_import_pending).then_some(CharacterMessage::Import),
        Tone::Secondary,
    );
    let ai_import_btn = action_icon(
        MessageIcon::Sparkle,
        if char_import_pending {
            "Importing…"
        } else {
            "Import with AI"
        },
        (!char_import_pending).then_some(CharacterMessage::AiImport),
        Tone::Secondary,
    );

    let actions_row = row![new_btn, import_btn, ai_import_btn]
        .spacing(6)
        .align_y(Alignment::Center);

    let search_bar = text_input("Search characters…", search_query)
        .on_input(CharacterMessage::SearchChanged)
        .size(14)
        .padding(Padding::from([8, 10]))
        .style(crate::ui::input_style)
        .width(Length::Fill);

    let query = search_query.trim().to_lowercase();
    let mut items = column![].spacing(4).width(Length::Fill);

    for c in characters {
        if !query.is_empty()
            && !c.name.to_lowercase().contains(&query)
            && !c.tags.iter().any(|t| t.to_lowercase().contains(&query))
            && !c.description.to_lowercase().contains(&query)
        {
            continue;
        }

        let is_selected = selected_id == Some(c.id.as_str());

        let avatar: Element<'a, CharacterMessage> = {
            let img_data = c
                .images
                .iter()
                .find(|img| c.default_image_id.as_deref() == Some(&img.id))
                .map(|img| &img.data)
                .or_else(|| c.images.first().map(|img| &img.data))
                .or(c.avatar.as_ref());

            if let Some(bytes) = img_data
                && let Some(handle) = image_handle(bytes)
            {
                image(handle).width(36).height(36).into()
            } else {
                let initial = c
                    .name
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string();
                container(text(initial).size(15))
                    .width(36)
                    .height(36)
                    .center_x(36)
                    .center_y(36)
                    .style(move |_| container::Style {
                        background: Some(crate::ui::raised_background(glass_mode)),
                        border: iced::border::Border {
                            color: crate::ui::border_color(glass_mode),
                            width: 0.0,
                            radius: 18.0.into(),
                        },
                        ..Default::default()
                    })
                    .into()
            }
        };

        let mut name_col = column![text(&c.name).size(13)].spacing(2);
        if c.is_public {
            name_col = name_col.push(text("Public").size(10).color(crate::ui::SECONDARY_TEXT));
        }

        let row_content = row![avatar, name_col]
            .spacing(10)
            .align_y(Alignment::Center)
            .width(Length::Fill);

        let item_button = button(row_content)
            .width(Length::Fill)
            .padding(Padding::from([6, 8]))
            .on_press(CharacterMessage::Select(c.id.clone()))
            .style(move |theme: &Theme, status: button::Status| {
                roster_button_style(theme, status, is_selected, glass_mode)
            });

        items = items.push(item_button);
    }

    column![
        actions_row,
        search_bar,
        scrollable(items).height(Length::Fill).width(Length::Fill)
    ]
    .spacing(8)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Labelled form control: a muted caption above the control.
fn labeled<'a>(
    label: &'a str,
    control: impl Into<Element<'a, CharacterMessage>>,
) -> Element<'a, CharacterMessage> {
    column![
        text(label).size(12).color(crate::ui::SECONDARY_TEXT),
        control.into(),
    ]
    .spacing(4)
    .width(Length::Fill)
    .into()
}

/// Single-line field; read-only when `on_input` is `None`.
fn line_input<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: Option<fn(String) -> CharacterMessage>,
) -> Element<'a, CharacterMessage> {
    text_input(placeholder, value)
        .on_input_maybe(on_input)
        .size(14)
        .padding(Padding::from([8, 10]))
        .style(crate::ui::input_style)
        .into()
}

/// Multi-line field that grows with its text up to the field's height limit,
/// then scrolls with a visible scrollbar.
///
/// `text_editor` has no scrollbar of its own, so it always takes its full text
/// height inside a capped `scrollable`.
fn long_input<'a>(
    editors: &'a DraftEditors,
    field: LongField,
    placeholder: &'a str,
    can_edit: bool,
) -> Element<'a, CharacterMessage> {
    let mut editor = text_editor(editors.content(field))
        .placeholder(placeholder)
        .size(14)
        .padding(Padding::from([8, 10]))
        .min_height(72.0)
        .style(|theme, status| {
            // The plate is drawn by the container so it does not scroll away.
            let mut style = crate::ui::editor_style(theme, status);
            style.background = Background::Color(Color::TRANSPARENT);
            style.border = iced::Border::default();
            style
        });
    if can_edit {
        editor = editor.on_action(move |action| CharacterMessage::Edit(field, action));
    }
    container(
        scrollable(editor)
            .id(field.scroll_id())
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new()
                    .width(6)
                    .scroller_width(6)
                    .margin(3),
            )),
    )
    .max_height(field.max_height())
    .style(|theme: &Theme| container::Style {
        background: Some(
            crate::ui::input_style(theme, iced::widget::text_input::Status::Active).background,
        ),
        border: iced::Border {
            radius: 12.0.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

/// Bold section heading with a muted explanation underneath.
fn section_heading<'a>(title: &'a str, help: &'a str) -> Element<'a, CharacterMessage> {
    column![
        text(title).size(14).font(crate::ui::SEMIBOLD),
        text(help).size(12).color(crate::ui::SECONDARY_TEXT),
    ]
    .spacing(2)
    .into()
}

/// Renders the character editor with identity fields, lorebook linking, image gallery, and actions.
#[must_use]
pub fn character_editor_view<'a>(
    draft: &'a DraftCharacter,
    editors: &'a DraftEditors,
    draft_world_ids: &'a HashSet<String>,
    worlds: &'a [World],
    portrait: ImageGeneration,
    glass_mode: bool,
) -> Element<'a, CharacterMessage> {
    iced::widget::responsive(move |size| {
        character_editor_at_width(
            draft,
            editors,
            draft_world_ids,
            worlds,
            portrait,
            glass_mode,
            size.width,
        )
    })
    .into()
}

#[allow(clippy::too_many_arguments)]
fn character_editor_at_width<'a>(
    draft: &'a DraftCharacter,
    editors: &'a DraftEditors,
    draft_world_ids: &'a HashSet<String>,
    worlds: &'a [World],
    portrait: ImageGeneration,
    glass_mode: bool,
    width: f32,
) -> Element<'a, CharacterMessage> {
    use crate::ui::{FooterIcon, MessageIcon, SidebarToggle, Tone, action_icon};
    let can_edit = draft.owned_by_user;
    let secondary = crate::ui::SECONDARY_TEXT;
    let edit = |message: fn(String) -> CharacterMessage| can_edit.then_some(message);
    let card = |content: Element<'a, CharacterMessage>| {
        crate::ui::card(content, glass_mode).width(Length::Fill)
    };

    let title_str = match (draft.id.is_some(), can_edit) {
        (false, _) => "Create character",
        (true, true) => "Edit character",
        (true, false) => "View character",
    };

    let mut header_col = column![text(title_str).size(18).font(crate::ui::BOLD)].spacing(2);
    if !can_edit {
        header_col = header_col.push(
            text("Public character · Only the owner can edit it.")
                .size(12)
                .color(secondary),
        );
    }

    let mut actions = row![].spacing(6).align_y(Alignment::Center);
    if let Some(id) = &draft.id {
        actions = actions.push(action_icon(
            MessageIcon::NewChat,
            "Start a chat",
            Some(CharacterMessage::Chat(id.clone())),
            Tone::Secondary,
        ));
        actions = actions.push(action_icon(
            FooterIcon::Memory,
            "Memories",
            Some(CharacterMessage::OpenMemories(id.clone())),
            Tone::Secondary,
        ));
    }
    if can_edit {
        actions = actions.push(action_icon(
            MessageIcon::Export,
            "Export character and images (.zip)",
            Some(CharacterMessage::Export),
            Tone::Secondary,
        ));
        if draft.id.is_some() {
            actions = actions.push(action_icon(
                MessageIcon::Delete,
                "Delete character",
                Some(CharacterMessage::Delete),
                Tone::Danger,
            ));
        }
        actions = actions.push(
            toggler(draft.is_public)
                .label("Public")
                .text_size(13)
                .on_toggle(CharacterMessage::TogglePublic),
        );
        // Images still generating would land after the save and be lost.
        let generating = matches!(
            portrait,
            ImageGeneration::Pending | ImageGeneration::Batch { .. }
        );
        actions = actions.push(action_icon(
            MessageIcon::Save,
            if generating {
                "Save after images finish"
            } else {
                "Save character"
            },
            (!generating).then_some(CharacterMessage::Save),
            Tone::Primary,
        ));
    }

    // Identity
    let age = labeled(
        "Age",
        line_input("Age", &draft.age, edit(CharacterMessage::AgeChanged)),
    );
    let gender = labeled(
        "Gender",
        line_input(
            "Gender",
            &draft.gender,
            edit(CharacterMessage::GenderChanged),
        ),
    );
    let race = labeled(
        "Race",
        line_input("Race", &draft.race, edit(CharacterMessage::RaceChanged)),
    );
    let identity_row: Element<'a, CharacterMessage> = if width < 420.0 {
        column![age, gender, race].spacing(10).into()
    } else {
        row![age, gender, race].spacing(10).into()
    };

    let identity = card(
        column![
            section_heading("Identity", "Who the character is at a glance."),
            labeled(
                "Name",
                line_input("Name", &draft.name, edit(CharacterMessage::NameChanged)),
            ),
            identity_row,
            labeled(
                "Tags",
                line_input(
                    "Comma separated, e.g. fantasy, mage",
                    &draft.tags,
                    edit(CharacterMessage::TagsChanged),
                ),
            ),
        ]
        .spacing(10)
        .into(),
    );

    let profile = card(
        column![
            section_heading(
                "Profile",
                "Sent to the model on every reply. Appearance also drives generated portraits.",
            ),
            labeled(
                "Description",
                long_input(
                    editors,
                    LongField::Description,
                    "Background, history, and role in the story",
                    can_edit,
                ),
            ),
            labeled(
                "Personality",
                long_input(
                    editors,
                    LongField::Personality,
                    "Temperament, habits, and how they speak",
                    can_edit,
                ),
            ),
            labeled(
                "Appearance",
                long_input(
                    editors,
                    LongField::Appearance,
                    "Hair, eyes, build, clothing",
                    can_edit,
                ),
            ),
        ]
        .spacing(10)
        .into(),
    );

    let roleplay = card(
        column![
            section_heading(
                "Roleplay",
                "Sets the scene and steers how the model plays this character.",
            ),
            labeled(
                "Scenario",
                long_input(
                    editors,
                    LongField::Scenario,
                    "Where and how the story begins",
                    can_edit,
                ),
            ),
            labeled(
                "System prompt",
                long_input(
                    editors,
                    LongField::SystemPrompt,
                    "Extra instructions for the model",
                    can_edit,
                ),
            ),
            labeled(
                "Example dialogue",
                long_input(
                    editors,
                    LongField::ExampleDialogue,
                    "Sample exchanges that show the character's voice",
                    can_edit,
                ),
            ),
            labeled(
                "Misc notes",
                long_input(
                    editors,
                    LongField::Misc,
                    "Anything else worth remembering",
                    can_edit,
                ),
            ),
        ]
        .spacing(10)
        .into(),
    );

    let mut lore_col = column![section_heading(
        "World lore",
        "Link this character to any worlds whose lore should be available during roleplay.",
    )]
    .spacing(8);

    if worlds.is_empty() {
        lore_col = lore_col.push(
            text("No worlds yet. Create one from the Worlds editor, then return here to link it.")
                .size(12)
                .color(secondary),
        );
    } else {
        let mut worlds_row = row![].spacing(14).align_y(Alignment::Center);
        for world in worlds {
            let linked = draft_world_ids.contains(&world.id);
            let world_id = world.id.clone();
            let cb = crate::ui::toggle_switch(linked, Some(world.name.as_str()), move |checked| {
                CharacterMessage::ToggleWorldLink {
                    world_id: world_id.clone(),
                    linked: checked,
                }
            });
            worlds_row = worlds_row.push(cb);
        }
        lore_col = lore_col.push(worlds_row.wrap());
    }

    let mut gallery_col = column![section_heading(
        "Character images",
        "Add PNG portraits and describe the expression or scene. Images are top-centered, cropped square, and shown as circles. Generated emotions use one seed and the Appearance field; review them, then save.",
    )]
    .spacing(8);

    if can_edit {
        let mut image_actions = row![
            action_icon(
                MessageIcon::Image,
                "Add images…",
                Some(CharacterMessage::AddImages),
                Tone::Secondary
            ),
            action_icon(
                MessageIcon::Folder,
                "Add a folder of images…",
                Some(CharacterMessage::AddImageFolder),
                Tone::Secondary
            ),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let cancel = || {
            action_icon(
                SidebarToggle::Collapse,
                "Cancel generation",
                Some(CharacterMessage::CancelImageGeneration),
                Tone::Secondary,
            )
        };
        match portrait {
            ImageGeneration::Unavailable => {}
            ImageGeneration::Ready => {
                let described = !draft.appearance.trim().is_empty();
                let missing = missing_emotions(draft).len();
                image_actions = image_actions
                    .push(action_icon(
                        MessageIcon::Sparkle,
                        if described {
                            "Generate portrait"
                        } else {
                            "Generate portrait (fill in Appearance first)"
                        },
                        described.then_some(CharacterMessage::GeneratePortrait),
                        Tone::Secondary,
                    ))
                    .push(action_icon(
                        MessageIcon::Emotions,
                        if described {
                            format!("Generate {missing} missing emotions")
                        } else {
                            "Generate emotions (fill in Appearance first)".to_owned()
                        },
                        (described && missing > 0).then_some(CharacterMessage::GenerateEmotions),
                        Tone::Secondary,
                    ));
            }
            ImageGeneration::Pending => {
                image_actions = image_actions
                    .push(cancel())
                    .push(text("Generating portrait…").size(13).color(secondary));
            }
            ImageGeneration::Batch { done, total } => {
                image_actions = image_actions.push(cancel()).push(
                    text(format!("Generating emotions {done}/{total}…"))
                        .size(13)
                        .color(secondary),
                );
            }
        }
        gallery_col = gallery_col.push(image_actions.wrap());
    }

    for img in &draft.images {
        let img_id = img.id.clone();
        let is_default = draft.default_image_id.as_deref() == Some(&img.id);

        let thumb: Element<'a, CharacterMessage> = if let Some(handle) = image_handle(&img.data) {
            image(handle).width(56).height(56).into()
        } else {
            container(text("No img").size(11).color(secondary))
                .width(56)
                .height(56)
                .center_x(56)
                .center_y(56)
                .into()
        };

        let id_for_label = img_id.clone();
        let label_input: Element<'a, CharacterMessage> = if can_edit {
            text_input("AI label (e.g. smiling)", &img.label)
                .on_input(move |label| CharacterMessage::ImageLabelChanged {
                    id: id_for_label.clone(),
                    label,
                })
                .size(13)
                .padding(Padding::from([6, 10]))
                .style(crate::ui::input_style)
                .into()
        } else {
            text(&img.label).size(13).into()
        };

        let mut img_actions = row![].spacing(6).align_y(Alignment::Center);
        if can_edit {
            img_actions = img_actions.push(action_icon(
                MessageIcon::Star,
                if is_default {
                    "Default portrait and icon"
                } else {
                    "Make default portrait and icon"
                },
                Some(CharacterMessage::SetDefaultImage(img_id.clone())),
                if is_default {
                    Tone::Primary
                } else {
                    Tone::Secondary
                },
            ));
            img_actions = img_actions.push(action_icon(
                MessageIcon::Delete,
                "Remove image",
                Some(CharacterMessage::DeleteImage(img_id)),
                Tone::Danger,
            ));
        }

        let img_card = container(
            row![thumb, labeled("AI label", label_input), img_actions,]
                .spacing(10)
                .align_y(Alignment::Center),
        )
        .padding(8)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(crate::ui::surface_background(glass_mode)),
            border: iced::border::Border {
                radius: 10.0.into(),
                ..Default::default()
            },
            ..Default::default()
        });
        gallery_col = gallery_col.push(img_card);
    }

    let sections = column![
        identity,
        profile,
        roleplay,
        card(lore_col.into()),
        card(gallery_col.into()),
    ]
    .spacing(12)
    .width(Length::Fill);

    // Keep actions beside the title when they fit; wrap below it otherwise.
    let header: Element<'a, CharacterMessage> = if width < 640.0 {
        column![header_col, actions.wrap()].spacing(10).into()
    } else {
        row![header_col, Space::new().width(Length::Fill), actions]
            .spacing(12)
            .align_y(Alignment::Center)
            .into()
    };

    let content = column![header, sections]
        .spacing(12)
        .padding(Padding {
            right: 12.0,
            ..Padding::ZERO
        })
        .width(Length::Fill);

    scrollable(content)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
}

/// Renders the complete Characters modal dialog including the character roster list and editor/viewer.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn character_dialog_view<'a>(
    characters: &'a [Character],
    draft: &'a DraftCharacter,
    editors: &'a DraftEditors,
    draft_world_ids: &'a HashSet<String>,
    worlds: &'a [World],
    search_query: &'a str,
    char_import_pending: bool,
    portrait: ImageGeneration,
    glass_mode: bool,
    compact: bool,
) -> Element<'a, CharacterMessage> {
    let header = row![
        text("Characters").size(22).font(crate::ui::BOLD),
        Space::new().width(Length::Fill),
        crate::ui::ghost_icon_button(
            crate::ui::SidebarToggle::Collapse,
            Some("Close"),
            CharacterMessage::CloseDialog,
        ),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill);

    let list_pane = character_list_view(
        characters,
        draft.id.as_deref(),
        search_query,
        char_import_pending,
        glass_mode,
    );

    let editor_pane = character_editor_view(
        draft,
        editors,
        draft_world_ids,
        worlds,
        portrait,
        glass_mode,
    );

    let body: Element<'a, CharacterMessage> = if compact {
        column![
            container(list_pane)
                .height(Length::FillPortion(1))
                .width(Length::Fill),
            container(editor_pane)
                .height(Length::FillPortion(2))
                .width(Length::Fill),
        ]
        .spacing(8)
        .into()
    } else {
        row![
            container(list_pane)
                .width(Length::Fixed(240.0))
                .height(Length::Fill),
            container(editor_pane)
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .spacing(12)
        .into()
    };

    container(
        column![header, body]
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
