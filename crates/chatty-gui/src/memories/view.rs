//! Memory manager dialog view.

use super::*;
use crate::ui::{self, MessageIcon, SidebarToggle, Tone, action_icon};
use chatty_protocol::{Character, Conversation, MemoryEntry, MemoryKind};
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, text, text_editor,
    text_input,
};
use iced::{Alignment, Background, Color, Element, Length, Padding, Theme};

/// Below this width filters and editor fields stack into one column.
const STACK_WIDTH: f32 = 600.0;

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

const KINDS: [MemoryKind; 4] = [
    MemoryKind::Fact,
    MemoryKind::Event,
    MemoryKind::Relationship,
    MemoryKind::Reflection,
];

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

/// Muted caption above a control.
fn labeled<'a, Message: 'a>(
    label: impl text::IntoFragment<'a>,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![
        text(label).size(12).color(ui::SECONDARY_TEXT),
        control.into()
    ]
    .spacing(4)
    .width(Length::Fill)
    .into()
}

/// Bold section heading with a muted explanation underneath.
fn section_heading<'a, Message: 'a>(title: &'a str, help: &'a str) -> Element<'a, Message> {
    column![
        text(title).size(14).font(ui::SEMIBOLD),
        text(help).size(12).color(ui::SECONDARY_TEXT),
    ]
    .spacing(2)
    .into()
}

/// Lays fields out side by side, or stacked when the dialog is narrow.
fn fields<'a, Message: 'a>(items: Vec<Element<'a, Message>>, narrow: bool) -> Element<'a, Message> {
    if narrow {
        column(items).spacing(10).into()
    } else {
        row(items).spacing(10).into()
    }
}

fn character_options(characters: &[Character], none_label: &str) -> Vec<CharacterOption> {
    std::iter::once(CharacterOption {
        id: None,
        label: none_label.into(),
    })
    .chain(characters.iter().map(|c| CharacterOption {
        id: Some(c.id.clone()),
        label: c.name.clone(),
    }))
    .collect()
}

fn conversation_options(
    conversations: &[Conversation],
    none_label: &str,
) -> Vec<ConversationOption> {
    std::iter::once(ConversationOption {
        id: None,
        label: none_label.into(),
    })
    .chain(conversations.iter().map(|c| ConversationOption {
        id: Some(c.id.clone()),
        label: c.title.clone(),
    }))
    .collect()
}

fn character_picker<'a, Message>(
    characters: &[Character],
    none_label: &str,
    selected: Option<&str>,
    on_select: fn(Option<String>) -> MemoryMessage,
) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let options = character_options(characters, none_label);
    let current = options
        .iter()
        .find(|o| o.id.as_deref() == selected)
        .cloned();
    pick_list(options, current, move |o: CharacterOption| {
        Message::from(on_select(o.id))
    })
    .text_size(14)
    .padding(Padding::from([8, 10]))
    .width(Length::Fill)
    .into()
}

fn conversation_picker<'a, Message>(
    conversations: &[Conversation],
    none_label: &str,
    selected: Option<&str>,
    on_select: fn(Option<String>) -> MemoryMessage,
) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let options = conversation_options(conversations, none_label);
    let current = options
        .iter()
        .find(|o| o.id.as_deref() == selected)
        .cloned();
    pick_list(options, current, move |o: ConversationOption| {
        Message::from(on_select(o.id))
    })
    .text_size(14)
    .padding(Padding::from([8, 10]))
    .width(Length::Fill)
    .into()
}

/// Renders the Memories management dialog.
pub fn memory_dialog_view<'a, Message>(view: &MemoryViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let view = view.clone();
    iced::widget::responsive(move |size| memory_dialog_at_width(&view, size.width < STACK_WIDTH))
        .into()
}

fn memory_dialog_at_width<'a, Message>(
    view: &MemoryViewState<'a>,
    narrow: bool,
) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let state = view.state;
    let glass_mode = view.glass_mode;
    let msg = |m: MemoryMessage| Message::from(m);

    let header = row![
        column![
            text("Memories").size(22).font(ui::BOLD),
            text("What characters remember across chats. Pinned memories are always sent.")
                .size(12)
                .color(ui::SECONDARY_TEXT),
        ]
        .spacing(2)
        .width(Length::Fill),
        ui::ghost_icon_button(
            SidebarToggle::Collapse,
            Some("Close"),
            msg(MemoryMessage::Close)
        ),
    ]
    .align_y(Alignment::Center);

    let can_condense = !state.condensing
        && can_condense_memories(
            view.memories,
            state.filter_character_id.as_deref(),
            state.filter_conversation_id.as_deref(),
            state.filter_kind,
        );
    let actions = row![
        action_icon(
            MessageIcon::Add,
            "New memory",
            Some(msg(MemoryMessage::AddMemory)),
            Tone::Primary,
        ),
        action_icon(
            MessageIcon::Sparkle,
            if view.can_extract {
                "Learn a memory from the open chat"
            } else {
                "Learn from chat (open a chat with a character first)"
            },
            view.can_extract
                .then(|| msg(MemoryMessage::ExtractFromChat)),
            Tone::Secondary,
        ),
        action_icon(
            MessageIcon::Merge,
            if state.condensing {
                "Condensing…"
            } else if can_condense {
                "Merge duplicate memories in this view"
            } else {
                "Merge duplicates (needs two unpinned memories of one kind)"
            },
            can_condense.then(|| msg(MemoryMessage::Condense)),
            Tone::Secondary,
        ),
        action_icon(
            MessageIcon::Regenerate,
            "Reload from server",
            Some(msg(MemoryMessage::Refresh)),
            Tone::Secondary,
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    let mut toolbar = row![actions].spacing(12).align_y(Alignment::Center);
    if let Some(status) = &state.status_message {
        toolbar = toolbar.push(text(status).size(12).color(ui::SECONDARY_TEXT));
    }

    // Filters
    let kind_options = std::iter::once(KindOption {
        kind: None,
        label: "All kinds",
    })
    .chain(KINDS.iter().map(|kind| KindOption {
        kind: Some(*kind),
        label: kind.label(),
    }))
    .collect::<Vec<_>>();
    let selected_kind = kind_options
        .iter()
        .copied()
        .find(|o| o.kind == state.filter_kind);
    let kind_picker = pick_list(kind_options, selected_kind, move |o: KindOption| {
        Message::from(MemoryMessage::FilterKind(o.kind))
    })
    .text_size(14)
    .padding(Padding::from([8, 10]))
    .width(Length::Fill);

    let mut search = row![
        text_input("e.g. stargazing", &state.search_query)
            .on_input(move |q| Message::from(MemoryMessage::SearchChanged(q)))
            .size(14)
            .padding(Padding::from([8, 10]))
            .style(ui::input_style)
            .width(Length::Fill)
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if !state.search_query.is_empty() {
        search = search.push(action_icon(
            SidebarToggle::Collapse,
            "Clear search",
            Some(msg(MemoryMessage::ClearSearch)),
            Tone::Secondary,
        ));
    }

    let filters = ui::card(
        column![
            fields(
                vec![
                    labeled(
                        "Character",
                        character_picker(
                            view.characters,
                            "All characters",
                            state.filter_character_id.as_deref(),
                            MemoryMessage::FilterCharacter,
                        ),
                    ),
                    labeled(
                        "Chat",
                        conversation_picker(
                            view.conversations,
                            "All chats",
                            state.filter_conversation_id.as_deref(),
                            MemoryMessage::FilterConversation,
                        ),
                    ),
                ],
                narrow,
            ),
            fields(
                vec![labeled("Kind", kind_picker), labeled("Search", search)],
                narrow
            ),
        ]
        .spacing(10),
        glass_mode,
    )
    .width(Length::Fill);

    let mut body = column![filters].spacing(12).width(Length::Fill);

    if let Some(draft) = &state.editing_memory {
        body = body.push(editor_card(view, draft, narrow));
    }

    // Memory list
    let filtered = filter_memories(
        view.memories,
        state.filter_character_id.as_deref(),
        state.filter_conversation_id.as_deref(),
        state.filter_kind,
        &state.search_query,
    );
    let summary = match filtered.len() {
        0 if view.memories.is_empty() => {
            "No memories yet. Characters learn them as you chat, or add one yourself.".to_owned()
        }
        0 => "No memories match these filters.".to_owned(),
        1 => "1 memory".to_owned(),
        count => format!("{count} memories"),
    };
    body = body.push(text(summary).size(12).color(ui::SECONDARY_TEXT));
    for memory in filtered {
        body = body.push(memory_card(view, memory));
    }

    let list = scrollable(container(body).padding(Padding {
        right: 14.0,
        ..Padding::ZERO
    }))
    .height(Length::Fill)
    .width(Length::Fill);

    container(
        column![header, toolbar.wrap(), list]
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(16)
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn editor_card<'a, Message>(
    view: &MemoryViewState<'a>,
    draft: &'a MemoryInput,
    narrow: bool,
) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let state = view.state;
    let msg = |m: MemoryMessage| Message::from(m);
    let title = if draft.id.is_some() {
        "Edit memory"
    } else {
        "New memory"
    };
    let has_content = !draft.content.trim().is_empty();

    let content = container(
        scrollable(
            text_editor(&state.editor)
                .placeholder("e.g. The user is afraid of deep water.")
                .on_action(move |action| Message::from(MemoryMessage::Edit(action)))
                .size(14)
                .padding(Padding::from([8, 10]))
                .min_height(72.0)
                .style(|theme, status| {
                    // The plate is drawn by the container so it does not scroll away.
                    let mut style = ui::editor_style(theme, status);
                    style.background = Background::Color(Color::TRANSPARENT);
                    style.border = iced::Border::default();
                    style
                }),
        )
        .id(editor_scroll_id())
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(6)
                .scroller_width(6)
                .margin(3),
        )),
    )
    .max_height(EDITOR_MAX_HEIGHT)
    .style(|theme: &Theme| container::Style {
        background: Some(ui::input_style(theme, text_input::Status::Active).background),
        border: iced::Border {
            radius: 12.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });

    let kind_picker = pick_list(
        KINDS.map(MemoryKindItem).to_vec(),
        Some(MemoryKindItem(draft.kind)),
        move |item: MemoryKindItem| Message::from(MemoryMessage::KindChanged(item.0)),
    )
    .text_size(14)
    .padding(Padding::from([8, 10]))
    .width(Length::Fill);

    let importance = slider(0..=MAX_IMPORTANCE, draft.importance, move |value| {
        Message::from(MemoryMessage::ImportanceChanged(value))
    })
    .step(5u8);

    let actions = row![
        Space::new().width(Length::Fill),
        action_icon(
            SidebarToggle::Collapse,
            "Discard changes",
            Some(msg(MemoryMessage::CancelEdit)),
            Tone::Secondary,
        ),
        action_icon(
            MessageIcon::Save,
            if has_content {
                "Save memory"
            } else {
                "Save memory (write the memory first)"
            },
            has_content.then(|| msg(MemoryMessage::SaveMemory)),
            Tone::Primary,
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    ui::card(
        column![
            section_heading(
                title,
                "One self-contained sentence. Higher importance is sent first when space is short.",
            ),
            labeled("Memory", content),
            fields(
                vec![
                    labeled("Kind", kind_picker),
                    labeled(
                        "Character",
                        character_picker(
                            view.characters,
                            "Any character",
                            draft.character_id.as_deref(),
                            MemoryMessage::CharacterChanged,
                        ),
                    ),
                    labeled(
                        "Chat",
                        conversation_picker(
                            view.conversations,
                            "All chats",
                            draft.conversation_id.as_deref(),
                            MemoryMessage::ConversationChanged,
                        ),
                    ),
                ],
                narrow,
            ),
            fields(
                vec![
                    labeled(format!("Importance · {}", draft.importance), importance),
                    labeled(
                        "Always send",
                        ui::toggle_switch(draft.pinned, Some("Pinned"), move |value| {
                            Message::from(MemoryMessage::PinnedToggled(value))
                        }),
                    ),
                ],
                narrow,
            ),
            actions,
        ]
        .spacing(10),
        view.glass_mode,
    )
    .width(Length::Fill)
    .into()
}

fn memory_card<'a, Message>(
    view: &MemoryViewState<'a>,
    memory: &'a MemoryEntry,
) -> Element<'a, Message>
where
    Message: 'a + Clone + From<MemoryMessage>,
{
    let msg = |m: MemoryMessage| Message::from(m);
    let editing = view
        .state
        .editing_memory
        .as_ref()
        .is_some_and(|draft| draft.id.as_deref() == Some(memory.id.as_str()));

    let mut details = vec![memory.kind.label(), memory.source.label()];
    let character = memory.character_id.as_ref().map(|id| {
        view.characters
            .iter()
            .find(|c| &c.id == id)
            .map_or("Unknown character", |c| c.name.as_str())
    });
    details.push(character.unwrap_or("Any character"));
    let chat = memory.conversation_id.as_ref().map(|id| {
        view.conversations
            .iter()
            .find(|c| &c.id == id)
            .map_or("Unknown chat", |c| c.title.as_str())
    });
    details.push(chat.unwrap_or("All chats"));
    let meta = format!("{} · Importance {}", details.join(" · "), memory.importance);

    let controls: Element<'a, Message> =
        if view.state.delete_confirm_id.as_deref() == Some(memory.id.as_str()) {
            row![
                text("Delete this memory?").size(13),
                button(text("Cancel").size(13))
                    .padding(Padding::from([6, 12]))
                    .style(ui::secondary_button_style())
                    .on_press(msg(MemoryMessage::CancelDelete)),
                button(text("Delete").size(13))
                    .padding(Padding::from([6, 12]))
                    .style(ui::danger_button_style())
                    .on_press(msg(MemoryMessage::ConfirmDelete(memory.id.clone()))),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                action_icon(
                    MessageIcon::Pin,
                    if memory.pinned {
                        "Unpin (send only when relevant)"
                    } else {
                        "Pin (always send)"
                    },
                    Some(msg(MemoryMessage::TogglePin(memory.id.clone()))),
                    if memory.pinned {
                        Tone::Primary
                    } else {
                        Tone::Secondary
                    },
                ),
                action_icon(
                    MessageIcon::Edit,
                    if editing {
                        "Editing above"
                    } else {
                        "Edit memory"
                    },
                    (!editing).then(|| msg(MemoryMessage::EditMemory(memory.id.clone()))),
                    Tone::Secondary,
                ),
                action_icon(
                    MessageIcon::Delete,
                    "Delete memory",
                    Some(msg(MemoryMessage::RequestDelete(memory.id.clone()))),
                    Tone::Danger,
                ),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        };

    ui::card(
        row![
            column![
                text(&memory.content).size(14),
                text(meta).size(12).color(ui::SECONDARY_TEXT),
            ]
            .spacing(4)
            .width(Length::Fill),
            controls,
        ]
        .spacing(12)
        .align_y(Alignment::Center),
        view.glass_mode,
    )
    .width(Length::Fill)
    .into()
}
