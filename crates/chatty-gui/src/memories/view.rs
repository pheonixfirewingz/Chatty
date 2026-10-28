//! Memory manager dialog view.

use super::*;
use crate::ui;
use chatty_protocol::{Character, Conversation, MemoryEntry, MemoryKind};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, text, text_input,
};
use iced::{Alignment, Element, Length, Padding};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterOption {
    pub id: Option<String>,
    pub label: String,
}

impl std::fmt::Display for CharacterOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationOption {
    pub id: Option<String>,
    pub label: String,
}

impl std::fmt::Display for ConversationOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindOption {
    pub kind: Option<MemoryKind>,
    pub label: &'static str,
}

impl std::fmt::Display for KindOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryKindItem(pub MemoryKind);

impl std::fmt::Display for MemoryKindItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.label())
    }
}

/// State passed to `memory_dialog_view`.
#[derive(Debug, Clone)]
pub struct MemoryViewState<'a> {
    pub memories: &'a [MemoryEntry],
    pub characters: &'a [Character],
    pub conversations: &'a [Conversation],
    pub state: &'a MemoryDialogState,
    pub can_extract: bool,
    pub glass_mode: bool,
}

impl<'a> MemoryViewState<'a> {
    pub fn new(
        memories: &'a [MemoryEntry],
        characters: &'a [Character],
        conversations: &'a [Conversation],
        state: &'a MemoryDialogState,
        can_extract: bool,
        glass_mode: bool,
    ) -> Self {
        Self {
            memories,
            characters,
            conversations,
            state,
            can_extract,
            glass_mode,
        }
    }
}

/// Renders the Iced 0.14 Memories management dialog.
pub fn memory_dialog_view<'a, Message>(view: &MemoryViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let state = view.state;

    // 1. Header
    let header = row![
        text("Memories").size(18),
        Space::new().width(Length::Fill),
        button(text("Refresh").size(12))
            .padding(Padding::from([3, 8]))
            .on_press(Message::from(MemoryMessage::Refresh)),
        button(text("✕").size(14))
            .padding(Padding::from([2, 8]))
            .on_press(Message::from(MemoryMessage::Close)),
    ]
    .spacing(8)
    .align_y(Alignment::Center);

    // 2. Filter Toolbar
    // Character filter options
    let mut char_options = vec![CharacterOption {
        id: None,
        label: "All characters".into(),
    }];
    for c in view.characters {
        char_options.push(CharacterOption {
            id: Some(c.id.clone()),
            label: c.name.clone(),
        });
    }
    let selected_char_opt = if let Some(ref cid) = state.filter_character_id {
        char_options
            .iter()
            .find(|o| o.id.as_deref() == Some(cid))
            .cloned()
    } else {
        Some(char_options[0].clone())
    };
    let char_picker = pick_list(char_options, selected_char_opt, |opt: CharacterOption| {
        Message::from(MemoryMessage::FilterCharacter(opt.id))
    });

    // Conversation filter options
    let mut conv_options = vec![ConversationOption {
        id: None,
        label: "All conversations".into(),
    }];
    for conv in view.conversations {
        conv_options.push(ConversationOption {
            id: Some(conv.id.clone()),
            label: conv.title.clone(),
        });
    }
    let selected_conv_opt = if let Some(ref cvid) = state.filter_conversation_id {
        conv_options
            .iter()
            .find(|o| o.id.as_deref() == Some(cvid))
            .cloned()
    } else {
        Some(conv_options[0].clone())
    };
    let conv_picker = pick_list(
        conv_options,
        selected_conv_opt,
        |opt: ConversationOption| Message::from(MemoryMessage::FilterConversation(opt.id)),
    );

    // Kind filter options
    let kind_options = vec![
        KindOption {
            kind: None,
            label: "All kinds",
        },
        KindOption {
            kind: Some(MemoryKind::Fact),
            label: "Facts",
        },
        KindOption {
            kind: Some(MemoryKind::Event),
            label: "Events",
        },
        KindOption {
            kind: Some(MemoryKind::Relationship),
            label: "Relationships",
        },
        KindOption {
            kind: Some(MemoryKind::Reflection),
            label: "Reflections",
        },
    ];
    let selected_kind_opt = kind_options
        .iter()
        .find(|o| o.kind == state.filter_kind)
        .copied()
        .unwrap_or(kind_options[0]);
    let kind_picker = pick_list(kind_options, Some(selected_kind_opt), |opt: KindOption| {
        Message::from(MemoryMessage::FilterKind(opt.kind))
    });

    let search_bar = text_input("Search memories…", &state.search_query)
        .on_input(|q| Message::from(MemoryMessage::SearchChanged(q)))
        .width(Length::Fixed(180.0));

    let mut filter_row = row![char_picker, conv_picker, kind_picker, search_bar,]
        .spacing(8)
        .align_y(Alignment::Center);

    if !state.search_query.is_empty() {
        filter_row = filter_row.push(
            button(text("Clear").size(12))
                .padding(Padding::from([3, 8]))
                .on_press(Message::from(MemoryMessage::ClearSearch)),
        );
    }

    let can_condense = !state.condensing
        && can_condense_memories(
            view.memories,
            state.filter_character_id.as_deref(),
            state.filter_conversation_id.as_deref(),
            state.filter_kind,
        );
    let condense_label = if state.condensing {
        "Condensing…"
    } else {
        "Condense memories"
    };

    let mut action_row = row![].spacing(8).align_y(Alignment::Center);
    if view.can_extract {
        action_row = action_row.push(
            button(text("Extract from chat").size(13))
                .padding(Padding::from([4, 10]))
                .on_press(Message::from(MemoryMessage::ExtractFromChat)),
        );
    }
    action_row = action_row.push(
        button(text(condense_label).size(13))
            .padding(Padding::from([4, 10]))
            .on_press_maybe(can_condense.then(|| Message::from(MemoryMessage::Condense))),
    );
    action_row = action_row.push(
        button(text("New memory").size(13))
            .padding(Padding::from([4, 10]))
            .on_press(Message::from(MemoryMessage::AddMemory)),
    );

    let mut toolbar_box = column![filter_row.wrap(), action_row.wrap()].spacing(6);
    if let Some(ref status) = state.status_message {
        toolbar_box = toolbar_box.push(text(status).size(12));
    }

    // 3. Delete Confirmation Prompt (if active)
    let delete_confirm_widget: Option<Element<'a, Message>> =
        state.delete_confirm_id.as_ref().map(|id| {
            let del_id = id.clone();
            let prompt_text =
                text("Are you sure you want to permanently delete this memory?").size(13);
            let confirm_btn = button(text("Delete").size(12))
                .padding(Padding::from([4, 10]))
                .on_press(Message::from(MemoryMessage::ConfirmDelete(del_id)));
            let cancel_btn = button(text("Cancel").size(12))
                .padding(Padding::from([4, 10]))
                .on_press(Message::from(MemoryMessage::CancelDelete));
            let actions = row![confirm_btn, cancel_btn].spacing(8);

            ui::card(column![prompt_text, actions].spacing(8), view.glass_mode)
                .width(Length::Fill)
                .into()
        });

    // 4. Memory Editor (if active)
    let editor_widget: Option<Element<'a, Message>> = state.editing_memory.as_ref().map(|draft| {
        let title_text = if draft.id.is_some() {
            "Edit memory"
        } else {
            "Create new memory"
        };

        // Kind selector
        let kind_items = vec![
            MemoryKindItem(MemoryKind::Fact),
            MemoryKindItem(MemoryKind::Event),
            MemoryKindItem(MemoryKind::Relationship),
            MemoryKindItem(MemoryKind::Reflection),
        ];
        let current_kind_item = MemoryKindItem(draft.kind);
        let kind_picker = pick_list(
            kind_items,
            Some(current_kind_item),
            |item: MemoryKindItem| Message::from(MemoryMessage::KindChanged(item.0)),
        );

        // Character selector
        let mut edit_char_options = vec![CharacterOption {
            id: None,
            label: "Global (no character)".into(),
        }];
        for c in view.characters {
            edit_char_options.push(CharacterOption {
                id: Some(c.id.clone()),
                label: c.name.clone(),
            });
        }
        let current_char_opt = if let Some(ref cid) = draft.character_id {
            edit_char_options
                .iter()
                .find(|o| o.id.as_deref() == Some(cid))
                .cloned()
        } else {
            Some(edit_char_options[0].clone())
        };
        let edit_char_picker = pick_list(
            edit_char_options,
            current_char_opt,
            |opt: CharacterOption| Message::from(MemoryMessage::CharacterChanged(opt.id)),
        );

        // Conversation selector
        let mut edit_conv_options = vec![ConversationOption {
            id: None,
            label: "Global (no conversation)".into(),
        }];
        for conv in view.conversations {
            edit_conv_options.push(ConversationOption {
                id: Some(conv.id.clone()),
                label: conv.title.clone(),
            });
        }
        let current_conv_opt = if let Some(ref cvid) = draft.conversation_id {
            edit_conv_options
                .iter()
                .find(|o| o.id.as_deref() == Some(cvid))
                .cloned()
        } else {
            Some(edit_conv_options[0].clone())
        };
        let edit_conv_picker = pick_list(
            edit_conv_options,
            current_conv_opt,
            |opt: ConversationOption| Message::from(MemoryMessage::ConversationChanged(opt.id)),
        );

        // Importance Slider
        let importance_slider = slider(1..=10, draft.importance, |val| {
            Message::from(MemoryMessage::ImportanceChanged(val))
        })
        .width(Length::Fixed(140.0));
        let importance_label = text(format!("Importance: {}/10", draft.importance)).size(12);

        // Pinned Toggle
        let pinned_toggle = ui::toggle_switch(draft.pinned, Some("Pinned"), |val| {
            Message::from(MemoryMessage::PinnedToggled(val))
        });

        let selectors_row = row![
            column![text("Kind").size(11), kind_picker].spacing(2),
            column![text("Character").size(11), edit_char_picker].spacing(2),
            column![text("Conversation").size(11), edit_conv_picker].spacing(2),
            column![importance_label, importance_slider].spacing(2),
            column![text("Always include").size(11), pinned_toggle].spacing(2),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        // Content Input
        let content_input = text_input("Memory content…", &draft.content)
            .on_input(|s| Message::from(MemoryMessage::ContentChanged(s)))
            .padding(Padding::from([6, 8]))
            .width(Length::Fill);

        // Save & Cancel Buttons
        let save_btn = button(text("Save memory").size(13))
            .padding(Padding::from([4, 12]))
            .on_press(Message::from(MemoryMessage::SaveMemory));
        let cancel_btn = button(text("Cancel").size(13))
            .padding(Padding::from([4, 10]))
            .on_press(Message::from(MemoryMessage::CancelEdit));
        let button_row = row![save_btn, cancel_btn].spacing(8);

        let editor_card = ui::card(
            column![
                text(title_text).size(14),
                selectors_row.wrap(),
                content_input,
                button_row,
            ]
            .spacing(8),
            view.glass_mode,
        )
        .width(Length::Fill);

        editor_card.into()
    });

    // 5. Memory Cards List
    let filtered = filter_memories(
        view.memories,
        state.filter_character_id.as_deref(),
        state.filter_conversation_id.as_deref(),
        state.filter_kind,
        &state.search_query,
    );

    let mut list_column = column![].spacing(8).width(Length::Fill);

    if filtered.is_empty() {
        list_column = list_column.push(text("No memories found matching the filters.").size(13));
    } else {
        list_column = list_column.push(text(format!("{} memories found", filtered.len())).size(12));

        for memory in filtered {
            let mid = memory.id.clone();
            let mid_del = memory.id.clone();

            let kind_badge = text(memory.kind.label()).size(12);
            let importance_badge = text(format!("★ {}", memory.importance)).size(12);
            let source_badge = text(memory.source.label()).size(11);

            let mut meta_row = row![kind_badge, importance_badge, source_badge,]
                .spacing(10)
                .align_y(Alignment::Center);

            if memory.pinned {
                meta_row = meta_row.push(text("📌 Pinned").size(11));
            }

            if memory.confidence > 0.0 {
                meta_row =
                    meta_row.push(text(format!("{:.0}% conf", memory.confidence * 100.0)).size(11));
            }

            meta_row = meta_row.push(Space::new().width(Length::Fill));

            let edit_btn = button(text("Edit").size(12))
                .padding(Padding::from([2, 8]))
                .on_press(Message::from(MemoryMessage::EditMemory(mid)));
            let delete_btn = button(text("Delete").size(12))
                .padding(Padding::from([2, 8]))
                .on_press(Message::from(MemoryMessage::RequestDelete(mid_del)));

            meta_row = meta_row.push(edit_btn);
            meta_row = meta_row.push(delete_btn);

            let content_display = text(&memory.content).size(13);

            let mut scope_parts = Vec::new();
            if let Some(ref cid) = memory.character_id {
                let char_name = view
                    .characters
                    .iter()
                    .find(|c| &c.id == cid)
                    .map(|c| c.name.as_str())
                    .unwrap_or("Unknown character");
                scope_parts.push(format!("Character: {char_name}"));
            }
            if let Some(ref cvid) = memory.conversation_id {
                let conv_title = view
                    .conversations
                    .iter()
                    .find(|cv| &cv.id == cvid)
                    .map(|cv| cv.title.as_str())
                    .unwrap_or("Unknown conversation");
                scope_parts.push(format!("Chat: {conv_title}"));
            }
            let scope_text = if scope_parts.is_empty() {
                "Scope: Global".to_string()
            } else {
                scope_parts.join(" · ")
            };
            let scope_label = text(scope_text).size(11);

            let card_inner = column![meta_row, content_display, scope_label,].spacing(6);

            let memory_card = ui::card(card_inner, view.glass_mode).width(Length::Fill);

            list_column = list_column.push(memory_card);
        }
    }

    // 6. Dialog Content Composition
    let mut dialog_content = column![header, toolbar_box,]
        .spacing(10)
        .width(Length::Fill);

    if let Some(confirm_w) = delete_confirm_widget {
        dialog_content = dialog_content.push(confirm_w);
    }

    if let Some(editor_w) = editor_widget {
        dialog_content = dialog_content.push(editor_w);
    }

    dialog_content = dialog_content.push(list_column);

    let scroll = scrollable(dialog_content)
        .width(Length::Fill)
        .height(Length::Shrink);

    container(scroll)
        .padding(20)
        .width(Length::Fill)
        .max_height(720.0)
        .into()
}
