//! World lore dialog view.

use super::*;
use crate::ui;
use chatty_protocol::{Character, World};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Padding};

/// Option item for the world dropdown picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldOption {
    pub id: Option<String>,
    pub label: String,
}

impl std::fmt::Display for WorldOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

/// State passed to `world_dialog_view`.
#[derive(Debug, Clone)]
pub struct WorldViewState<'a> {
    pub worlds: &'a [World],
    pub world_draft: &'a World,
    pub characters: &'a [Character],
    pub entry_search: &'a str,
    pub import_pending: bool,
    pub import_elapsed_secs: u64,
    pub import_notice: Option<&'a str>,
    pub glass_mode: bool,
}

/// Renders the Iced 0.14 World Lore dialog.
pub fn world_dialog_view<'a, Message>(state: &WorldViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<WorldMessage>,
{
    // 1. Header row
    let header = row![
        text("World lore").size(18),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding(Padding::from([2, 8]))
            .on_press(Message::from(WorldMessage::Close)),
    ]
    .align_y(Alignment::Center);

    // 2. World picker and CRUD toolbar
    let mut world_options: Vec<WorldOption> = vec![WorldOption {
        id: None,
        label: "New world".to_string(),
    }];
    for world in state.worlds {
        world_options.push(WorldOption {
            id: Some(world.id.clone()),
            label: world.name.clone(),
        });
    }

    let selected_option = if state.world_draft.id.is_empty() {
        Some(WorldOption {
            id: None,
            label: "New world".to_string(),
        })
    } else {
        Some(WorldOption {
            id: Some(state.world_draft.id.clone()),
            label: state.world_draft.name.clone(),
        })
    };

    let world_picker = pick_list(world_options, selected_option, |opt: WorldOption| {
        Message::from(WorldMessage::SelectWorld(opt.id))
    });

    let new_world_btn = button(text("New world")).on_press(Message::from(WorldMessage::NewWorld));

    let import_btn = if state.import_pending {
        button(text("Importing…"))
    } else {
        button(text("Import SillyTavern")).on_press(Message::from(WorldMessage::ImportLorebook))
    };

    let save_btn = button(text("Save world")).on_press(Message::from(WorldMessage::SaveWorld));

    let mut toolbar = row![world_picker, new_world_btn, import_btn, save_btn,]
        .spacing(8)
        .align_y(Alignment::Center);

    if !state.world_draft.id.is_empty() {
        let delete_btn =
            button(text("Delete world")).on_press(Message::from(WorldMessage::DeleteWorld));
        toolbar = toolbar.push(delete_btn);
    }

    // 3. Status indicator
    let is_saved = state.worlds.iter().any(|w| w == state.world_draft);
    let status_label = if is_saved {
        text("Saved").size(12)
    } else {
        text("Unsaved changes — save to apply").size(12)
    };

    let mut status_column = column![status_label].spacing(4);

    if state.import_pending {
        status_column = status_column.push(
            text(format!(
                "Converting lore with AI… {}s elapsed",
                state.import_elapsed_secs
            ))
            .size(13),
        );
        status_column = status_column.push(
            text("The model is reading, classifying, and formatting every entry. Large lorebooks or a cold model can take several minutes.")
                .size(11),
        );
    } else if let Some(notice) = state.import_notice {
        status_column = status_column.push(text(notice).size(13));
    }

    // 4. World details: name and character links
    let name_label = text("World name").size(13);
    let name_input = text_input("World name", &state.world_draft.name)
        .on_input(|s| Message::from(WorldMessage::NameChanged(s)));

    let characters_label = text("Linked characters").size(13);
    let characters_sub =
        text("This world is available when a linked character speaks in your chats.").size(11);

    let mut char_links_col = column![].spacing(6);
    for character in state.characters {
        let linked = state.world_draft.character_ids.contains(&character.id);
        let char_id = character.id.clone();
        let toggle = ui::toggle_switch(linked, Some(character.name.as_str()), move |val| {
            Message::from(WorldMessage::ToggleCharacterLink {
                character_id: char_id.clone(),
                linked: val,
            })
        });
        char_links_col = char_links_col.push(toggle);
    }

    // 5. Lore entries section
    let lore_intro = text("Common knowledge is selected first. Other facts activate from keywords in the last six messages. Priority decides what fits within the context budget.").size(11);

    let add_entry_btn =
        button(text("Add lore entry")).on_press(Message::from(WorldMessage::AddEntry));

    let search_input = text_input(
        "Search entries (title, content, or keyword)",
        state.entry_search,
    )
    .on_input(|s| Message::from(WorldMessage::SearchChanged(s)))
    .width(Length::Fixed(260.0));

    let mut entry_toolbar = row![add_entry_btn, search_input]
        .spacing(8)
        .align_y(Alignment::Center);
    if !state.entry_search.is_empty() {
        entry_toolbar = entry_toolbar
            .push(button(text("Clear")).on_press(Message::from(WorldMessage::ClearSearch)));
    }

    let matching = filter_entries(&state.world_draft.entries, state.entry_search);
    let mut entries_col = column![].spacing(8);

    if !state.entry_search.is_empty() {
        entries_col = entries_col.push(
            text(format!(
                "{} of {} entries shown",
                matching.len(),
                state.world_draft.entries.len()
            ))
            .size(12),
        );
    }

    for (index, fact) in matching {
        let enabled_toggle = ui::toggle_switch(fact.enabled, Some("Enabled"), move |val| {
            Message::from(WorldMessage::EntryEnabledToggled {
                index,
                enabled: val,
            })
        });

        let common_knowledge_toggle = ui::toggle_switch(
            fact.common_knowledge,
            Some("Common knowledge"),
            move |val| {
                Message::from(WorldMessage::EntryCommonKnowledgeToggled {
                    index,
                    common_knowledge: val,
                })
            },
        );

        let prio_down = button(text("-").size(12))
            .padding(Padding::from([2, 6]))
            .on_press(Message::from(WorldMessage::EntryPriorityChanged {
                index,
                priority: fact.priority.saturating_sub(1),
            }));

        let prio_text = text(format!("Priority: {}", fact.priority)).size(13);

        let prio_up = button(text("+").size(12))
            .padding(Padding::from([2, 6]))
            .on_press(Message::from(WorldMessage::EntryPriorityChanged {
                index,
                priority: fact.priority.saturating_add(1),
            }));

        let remove_btn = button(text("Remove").size(12))
            .padding(Padding::from([3, 8]))
            .on_press(Message::from(WorldMessage::RemoveEntry(index)));

        let card_header = row![
            enabled_toggle,
            common_knowledge_toggle,
            prio_down,
            prio_text,
            prio_up,
            Space::new().width(Length::Fill),
            remove_btn,
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let title_input = text_input("Title", &fact.title)
            .on_input(move |t| Message::from(WorldMessage::EntryTitleChanged { index, title: t }));

        let content_input = text_input("Lore content...", &fact.content).on_input(move |c| {
            Message::from(WorldMessage::EntryContentChanged { index, content: c })
        });

        let mut card_col = column![card_header, title_input].spacing(6);

        if !fact.common_knowledge {
            let kw_str = fact.keywords.join(", ");
            let kw_input = text_input("Keywords (comma separated, case insensitive)", &kw_str)
                .on_input(move |k| {
                    Message::from(WorldMessage::EntryKeywordsChanged { index, keywords: k })
                });
            card_col = card_col.push(kw_input);
        }

        card_col = card_col.push(content_input);

        let card_widget = ui::card(card_col, state.glass_mode).width(Length::Fill);
        entries_col = entries_col.push(card_widget);
    }

    // Combine everything into a scrollable dialog body
    let dialog_content = column![
        header,
        toolbar.wrap(),
        status_column,
        name_label,
        name_input,
        characters_label,
        characters_sub,
        char_links_col,
        lore_intro,
        entry_toolbar.wrap(),
        entries_col,
    ]
    .spacing(10)
    .width(Length::Fill);

    let scroll = scrollable(dialog_content)
        .width(Length::Fill)
        .height(Length::Shrink);

    container(scroll)
        .padding(20)
        .width(Length::Fill)
        .max_height(720.0)
        .into()
}
