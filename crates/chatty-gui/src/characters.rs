//! Character roster, editing views, and SillyTavern card import/export built with Iced 0.14.
//!
//! Provides the data model (`DraftCharacter`), Elm-architecture messages (`CharacterMessage`),
//! Iced view components (`character_dialog_view`, `character_list_view`, `character_editor_view`),
//! image normalization, image handle caching, and SillyTavern JSON/PNG card parsing and export.

use chatty_protocol::{Character, CharacterImage, CharacterInput, World};
use iced::widget::{
    button, column, container, image, row, scrollable, text, text_input, toggler, Space,
};
use iced::{Alignment, Background, Color, Element, Length, Padding, Theme};
use std::collections::HashSet;

// -----------------------------------------------------------------------------
// Data Model & Conversions
// -----------------------------------------------------------------------------

/// In-progress character draft representation used by the editor and card import/export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftCharacter {
    pub id: Option<String>,
    pub name: String,
    pub description: String,
    pub personality: String,
    pub scenario: String,
    pub system_prompt: String,
    pub example_dialogue: String,
    pub appearance: String,
    pub age: String,
    pub gender: String,
    pub race: String,
    pub misc: String,
    pub tags: String,
    pub avatar: Option<Vec<u8>>,
    pub images: Vec<CharacterImage>,
    pub default_image_id: Option<String>,
    pub is_public: bool,
    pub owned_by_user: bool,
}

impl Default for DraftCharacter {
    fn default() -> Self {
        Self {
            id: None,
            name: String::new(),
            description: String::new(),
            personality: String::new(),
            scenario: String::new(),
            system_prompt: String::new(),
            example_dialogue: String::new(),
            appearance: String::new(),
            age: String::new(),
            gender: String::new(),
            race: String::new(),
            misc: String::new(),
            tags: String::new(),
            avatar: None,
            images: Vec::new(),
            default_image_id: None,
            is_public: false,
            owned_by_user: true,
        }
    }
}

impl From<&Character> for DraftCharacter {
    fn from(c: &Character) -> Self {
        Self {
            id: Some(c.id.clone()),
            name: c.name.clone(),
            description: c.description.clone(),
            personality: c.personality.clone(),
            scenario: c.scenario.clone(),
            system_prompt: c.system_prompt.clone(),
            example_dialogue: c.example_dialogue.clone(),
            appearance: c.appearance.clone(),
            age: c.age.clone(),
            gender: c.gender.clone(),
            race: c.race.clone(),
            misc: c.misc.clone(),
            tags: c.tags.join(", "),
            avatar: c.avatar.clone(),
            images: c.images.clone(),
            default_image_id: c.default_image_id.clone(),
            is_public: c.is_public,
            owned_by_user: c.owned_by_user,
        }
    }
}

impl From<Character> for DraftCharacter {
    fn from(c: Character) -> Self {
        Self::from(&c)
    }
}

impl From<&CharacterInput> for DraftCharacter {
    fn from(c: &CharacterInput) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            description: c.description.clone(),
            personality: c.personality.clone(),
            scenario: c.scenario.clone(),
            system_prompt: c.system_prompt.clone(),
            example_dialogue: c.example_dialogue.clone(),
            appearance: c.appearance.clone(),
            age: c.age.clone(),
            gender: c.gender.clone(),
            race: c.race.clone(),
            misc: c.misc.clone(),
            tags: c.tags.join(", "),
            avatar: c.avatar.clone(),
            images: c.images.clone(),
            default_image_id: c.default_image_id.clone(),
            is_public: c.is_public,
            owned_by_user: c.owned_by_user,
        }
    }
}

impl From<CharacterInput> for DraftCharacter {
    fn from(c: CharacterInput) -> Self {
        Self::from(&c)
    }
}

impl From<&DraftCharacter> for CharacterInput {
    fn from(d: &DraftCharacter) -> Self {
        Self {
            id: d.id.clone(),
            name: d.name.trim().to_owned(),
            description: d.description.clone(),
            personality: d.personality.clone(),
            scenario: d.scenario.clone(),
            system_prompt: d.system_prompt.clone(),
            example_dialogue: d.example_dialogue.clone(),
            appearance: d.appearance.clone(),
            age: d.age.clone(),
            gender: d.gender.clone(),
            race: d.race.clone(),
            misc: d.misc.clone(),
            tags: d
                .tags
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            avatar: if d.images.is_empty() {
                d.avatar.clone()
            } else {
                None
            },
            images: d.images.clone(),
            default_image_id: d.default_image_id.clone(),
            is_public: d.is_public,
            owned_by_user: d.owned_by_user,
        }
    }
}

impl From<DraftCharacter> for CharacterInput {
    fn from(d: DraftCharacter) -> Self {
        CharacterInput::from(&d)
    }
}

impl DraftCharacter {
    /// Convert this `DraftCharacter` into a protocol `CharacterInput`.
    #[must_use]
    pub fn to_character_input(&self) -> CharacterInput {
        CharacterInput::from(self)
    }

    /// Applies form updates or image modifications from a `CharacterMessage`.
    pub fn update(&mut self, message: &CharacterMessage) {
        match message {
            CharacterMessage::NameChanged(val) => self.name = val.clone(),
            CharacterMessage::DescriptionChanged(val) => self.description = val.clone(),
            CharacterMessage::PersonalityChanged(val) => self.personality = val.clone(),
            CharacterMessage::ScenarioChanged(val) => self.scenario = val.clone(),
            CharacterMessage::SystemPromptChanged(val) => self.system_prompt = val.clone(),
            CharacterMessage::ExampleDialogueChanged(val) => self.example_dialogue = val.clone(),
            CharacterMessage::AppearanceChanged(val) => self.appearance = val.clone(),
            CharacterMessage::AgeChanged(val) => self.age = val.clone(),
            CharacterMessage::GenderChanged(val) => self.gender = val.clone(),
            CharacterMessage::RaceChanged(val) => self.race = val.clone(),
            CharacterMessage::MiscChanged(val) => self.misc = val.clone(),
            CharacterMessage::TagsChanged(val) => self.tags = val.clone(),
            CharacterMessage::TogglePublic(val) => self.is_public = *val,
            CharacterMessage::SetDefaultImage(id) => self.default_image_id = Some(id.clone()),
            CharacterMessage::DeleteImage(id) => {
                self.images.retain(|img| &img.id != id);
                if self.default_image_id.as_deref() == Some(id.as_str()) {
                    self.default_image_id = self.images.first().map(|img| img.id.clone());
                }
            }
            CharacterMessage::ImageLabelChanged { id, label } => {
                if let Some(img) = self.images.iter_mut().find(|img| &img.id == id) {
                    img.label = label.clone();
                }
            }
            _ => {}
        }
    }
}

// -----------------------------------------------------------------------------
// SillyTavern JSON & PNG Card Parsing / Export
// -----------------------------------------------------------------------------

/// Parse a SillyTavern JSON v2 character card or fallback JSON object into a `DraftCharacter`.
#[must_use]
pub fn draft_from_card(root: serde_json::Value) -> Option<DraftCharacter> {
    let d = root.get("data").unwrap_or(&root);
    let ext = d.pointer("/extensions/chatty");
    Some(DraftCharacter {
        id: None,
        name: d.get("name")?.as_str()?.into(),
        description: str_field(d, "description"),
        personality: str_field(d, "personality"),
        scenario: str_field(d, "scenario"),
        system_prompt: str_field(d, "system_prompt"),
        example_dialogue: str_field(d, "mes_example"),
        appearance: ext.map(|e| str_field(e, "appearance")).unwrap_or_default(),
        age: ext.map(|e| str_field(e, "age")).unwrap_or_default(),
        gender: ext.map(|e| str_field(e, "gender")).unwrap_or_default(),
        race: ext.map(|e| str_field(e, "race")).unwrap_or_default(),
        misc: ext.map(|e| str_field(e, "misc")).unwrap_or_default(),
        tags: d
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        avatar: None,
        images: vec![],
        default_image_id: None,
        is_public: false,
        owned_by_user: true,
    })
}

fn str_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .into()
}

/// Export a `DraftCharacter` to a SillyTavern JSON v2 character card object.
#[must_use]
pub fn export_card_json(draft: &DraftCharacter) -> serde_json::Value {
    let tags: Vec<&str> = draft
        .tags
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    serde_json::json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "data": {
            "name": draft.name,
            "description": draft.description,
            "personality": draft.personality,
            "scenario": draft.scenario,
            "first_mes": "",
            "mes_example": draft.example_dialogue,
            "system_prompt": draft.system_prompt,
            "tags": tags,
            "extensions": {
                "chatty": {
                    "appearance": draft.appearance,
                    "age": draft.age,
                    "gender": draft.gender,
                    "race": draft.race,
                    "misc": draft.misc,
                }
            }
        }
    })
}

/// Convenience alias for `export_card_json`.
#[must_use]
pub fn draft_to_card(draft: &DraftCharacter) -> serde_json::Value {
    export_card_json(draft)
}

/// Export a `DraftCharacter` as formatted JSON bytes.
pub fn export_card_bytes(draft: &DraftCharacter) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(&export_card_json(draft))
}

/// Extracts and base64-decodes the `tEXt` chunk with keyword `chara` from PNG image bytes.
#[must_use]
pub fn png_card_json(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        if at + 12 + len > bytes.len() {
            return None;
        }
        if kind == b"tEXt" {
            let data = &bytes[at + 8..at + 8 + len];
            if let Some(zero) = data.iter().position(|b| *b == 0) {
                if &data[..zero] == b"chara" {
                    let text = std::str::from_utf8(&data[zero + 1..]).ok()?;
                    return chatty_protocol::util::base64::decode(text);
                }
            }
        }
        at += 12 + len;
    }
    None
}

// -----------------------------------------------------------------------------
// Image Normalization & Caching
// -----------------------------------------------------------------------------

/// Normalizes a PNG character portrait: verifies 128..=512 dimensions (max 4096 px source)
/// and crops horizontally centered to a top-aligned square if needed.
pub fn normalize_character_image_png(
    source: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    let decoded = ::image::load_from_memory_with_format(source, ::image::ImageFormat::Png)
        .map_err(|_| "must be a valid PNG".to_owned())?;
    let (width, height) = (decoded.width(), decoded.height());
    let side = width.min(height);
    if !(128..=512).contains(&side) || width > 4096 || height > 4096 {
        return Err(
            "the cropped portrait must be 128×128 to 512×512 (source sides may be up to 4096 px)"
                .into(),
        );
    }
    if width == height {
        return Ok(source.to_vec());
    }
    let left = (width - side) / 2;
    let cropped = decoded.crop_imm(left, 0, side, side);
    let mut output = std::io::Cursor::new(Vec::new());
    cropped
        .write_to(&mut output, ::image::ImageFormat::Png)
        .map_err(|_| "could not normalize PNG portrait".to_owned())?;
    Ok(output.into_inner())
}

/// Creates an `Option<iced::widget::image::Handle>` from image bytes if non-empty.
#[must_use]
pub fn image_handle(bytes: &[u8]) -> Option<iced::widget::image::Handle> {
    if bytes.is_empty() {
        None
    } else {
        Some(iced::widget::image::Handle::from_bytes(bytes.to_vec()))
    }
}

/// Alias for `image_handle`.
#[must_use]
pub fn image_handle_from_bytes(bytes: &[u8]) -> Option<iced::widget::image::Handle> {
    image_handle(bytes)
}

/// Cache mapping image IDs/hashes to `iced::widget::image::Handle` instances.
#[derive(Default, Debug, Clone)]
pub struct CharacterImageCache {
    handles: std::collections::HashMap<String, iced::widget::image::Handle>,
}

impl CharacterImageCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_insert(&mut self, id: &str, bytes: &[u8]) -> Option<iced::widget::image::Handle> {
        if bytes.is_empty() {
            return None;
        }
        if let Some(h) = self.handles.get(id) {
            return Some(h.clone());
        }
        let handle = iced::widget::image::Handle::from_bytes(bytes.to_vec());
        self.handles.insert(id.to_string(), handle.clone());
        Some(handle)
    }

    pub fn clear(&mut self) {
        self.handles.clear();
    }
}

// -----------------------------------------------------------------------------
// Elm Messages / Actions
// -----------------------------------------------------------------------------

/// User actions within the character dialog, roster, and character editor.
#[derive(Debug, Clone, PartialEq)]
pub enum CharacterMessage {
    // Form field updates
    NameChanged(String),
    DescriptionChanged(String),
    PersonalityChanged(String),
    ScenarioChanged(String),
    SystemPromptChanged(String),
    ExampleDialogueChanged(String),
    AppearanceChanged(String),
    AgeChanged(String),
    GenderChanged(String),
    RaceChanged(String),
    MiscChanged(String),
    TagsChanged(String),
    TogglePublic(bool),

    // Selection and CRUD
    Select(String),
    New,
    Save,
    Delete,
    Export,
    Import,
    AiImport,
    CloseDialog,

    // Image gallery actions
    AddImages,
    AddImageFolder,
    SetDefaultImage(String),
    DeleteImage(String),
    ImageLabelChanged { id: String, label: String },

    // Navigation
    Chat(String),
    OpenMemories(String),

    // Lorebook / World linking
    ToggleWorldLink { world_id: String, linked: bool },

    // Search filter
    SearchChanged(String),
}

/// Alias for `CharacterMessage`.
pub type CharacterAction = CharacterMessage;

// -----------------------------------------------------------------------------
// Iced 0.14 View Functions
// -----------------------------------------------------------------------------

fn roster_button_style(
    theme: &Theme,
    status: button::Status,
    selected: bool,
    light_mode: bool,
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
                crate::ui::raised_background(light_mode, glass_mode),
                crate::ui::border_color(light_mode, glass_mode),
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
            button::Status::Disabled => (
                crate::ui::raised_background(light_mode, glass_mode),
                Color::TRANSPARENT,
            ),
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
    light_mode: bool,
    glass_mode: bool,
) -> Element<'a, CharacterMessage> {
    let new_btn = button(text("New").size(13))
        .padding(Padding::from([5, 10]))
        .on_press(CharacterMessage::New);

    let import_btn = if char_import_pending {
        button(text("Importing…").size(13)).padding(Padding::from([5, 10]))
    } else {
        button(text("Import").size(13))
            .padding(Padding::from([5, 10]))
            .on_press(CharacterMessage::Import)
    };

    let ai_import_btn = if char_import_pending {
        button(text("AI Import").size(13)).padding(Padding::from([5, 10]))
    } else {
        button(text("AI Import").size(13))
            .padding(Padding::from([5, 10]))
            .on_press(CharacterMessage::AiImport)
    };

    let actions_row = row![new_btn, import_btn, ai_import_btn]
        .spacing(6)
        .align_y(Alignment::Center);

    let search_bar = text_input("Search characters…", search_query)
        .on_input(CharacterMessage::SearchChanged)
        .padding(Padding::from([6, 8]))
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
                let initial = c.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                container(text(initial).size(15))
                    .width(36)
                    .height(36)
                    .center_x(36)
                    .center_y(36)
                    .style(move |_| container::Style {
                        background: Some(crate::ui::raised_background(light_mode, glass_mode)),
                        border: iced::border::Border {
                            color: crate::ui::border_color(light_mode, glass_mode),
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
            name_col = name_col.push(text("Public").size(10));
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
                roster_button_style(theme, status, is_selected, light_mode, glass_mode)
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

/// Renders the character editor with identity fields, lorebook linking, image gallery, and actions.
#[must_use]
pub fn character_editor_view<'a>(
    draft: &'a DraftCharacter,
    draft_world_ids: &'a HashSet<String>,
    worlds: &'a [World],
    light_mode: bool,
    glass_mode: bool,
) -> Element<'a, CharacterMessage> {
    iced::widget::responsive(move |size| {
        character_editor_at_width(
            draft,
            draft_world_ids,
            worlds,
            light_mode,
            glass_mode,
            size.width,
        )
    })
    .into()
}

fn character_editor_at_width<'a>(
    draft: &'a DraftCharacter,
    draft_world_ids: &'a HashSet<String>,
    worlds: &'a [World],
    light_mode: bool,
    glass_mode: bool,
    width: f32,
) -> Element<'a, CharacterMessage> {
    let can_edit = draft.owned_by_user;

    let title_str = match (draft.id.is_some(), can_edit) {
        (false, _) => "Create character",
        (true, true) => "Edit character",
        (true, false) => "View character",
    };

    let mut header_col = column![text(title_str).size(18)].spacing(2);
    if !can_edit {
        header_col = header_col.push(text("Public character · Only the owner can edit it.").size(12));
    }

    let mut actions = row![].spacing(8).align_y(Alignment::Center);
    if let Some(id) = &draft.id {
        actions = actions.push(
            button(text("Chat").size(13))
                .padding(Padding::from([5, 12]))
                .on_press(CharacterMessage::Chat(id.clone())),
        );
        actions = actions.push(
            button(text("Memories").size(13))
                .padding(Padding::from([5, 12]))
                .on_press(CharacterMessage::OpenMemories(id.clone())),
        );
    }
    if can_edit {
        actions = actions.push(
            toggler(draft.is_public)
                .label("Public")
                .on_toggle(CharacterMessage::TogglePublic),
        );
        actions = actions.push(
            button(text("Save").size(13))
                .padding(Padding::from([5, 12]))
                .on_press(CharacterMessage::Save),
        );
        actions = actions.push(
            button(text("Export").size(13))
                .padding(Padding::from([5, 12]))
                .on_press(CharacterMessage::Export),
        );
        if draft.id.is_some() {
            actions = actions.push(
                button(text("Delete").size(13))
                    .padding(Padding::from([5, 12]))
                    .on_press(CharacterMessage::Delete),
            );
        }
    }

    // Left column: Identity & Core Fields
    let name_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Name", &draft.name)
            .on_input(CharacterMessage::NameChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Name", &draft.name)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let age_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Age", &draft.age)
            .on_input(CharacterMessage::AgeChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Age", &draft.age)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let gender_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Gender", &draft.gender)
            .on_input(CharacterMessage::GenderChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Gender", &draft.gender)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let race_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Race", &draft.race)
            .on_input(CharacterMessage::RaceChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Race", &draft.race)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let identity_row: Element<'a, CharacterMessage> = if width < 400.0 {
        column![
            column![text("Age").size(12), age_input],
            column![text("Gender").size(12), gender_input],
            column![text("Race").size(12), race_input],
        ]
        .spacing(8)
        .into()
    } else {
        row![
            column![text("Age").size(12), age_input].width(Length::FillPortion(1)),
            column![text("Gender").size(12), gender_input].width(Length::FillPortion(1)),
            column![text("Race").size(12), race_input].width(Length::FillPortion(1)),
        ]
        .spacing(8)
        .into()
    };

    let tags_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Tags (comma separated)", &draft.tags)
            .on_input(CharacterMessage::TagsChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Tags (comma separated)", &draft.tags)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let desc_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Description", &draft.description)
            .on_input(CharacterMessage::DescriptionChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Description", &draft.description)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let pers_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Personality", &draft.personality)
            .on_input(CharacterMessage::PersonalityChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Personality", &draft.personality)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let app_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Appearance", &draft.appearance)
            .on_input(CharacterMessage::AppearanceChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Appearance", &draft.appearance)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let left_col = column![
        column![text("Name").size(12), name_input].spacing(2),
        identity_row,
        column![text("Tags").size(12), tags_input].spacing(2),
        column![text("Description").size(12), desc_input].spacing(2),
        column![text("Personality").size(12), pers_input].spacing(2),
        column![text("Appearance").size(12), app_input].spacing(2),
    ]
    .spacing(8)
    .width(Length::FillPortion(1));

    // Right column: Scenario, System Prompt, Lore, Images
    let scen_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Scenario", &draft.scenario)
            .on_input(CharacterMessage::ScenarioChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Scenario", &draft.scenario)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let sys_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("System prompt", &draft.system_prompt)
            .on_input(CharacterMessage::SystemPromptChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("System prompt", &draft.system_prompt)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let ex_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Example dialogue", &draft.example_dialogue)
            .on_input(CharacterMessage::ExampleDialogueChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Example dialogue", &draft.example_dialogue)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let misc_input: Element<'a, CharacterMessage> = if can_edit {
        text_input("Misc notes", &draft.misc)
            .on_input(CharacterMessage::MiscChanged)
            .padding(Padding::from([6, 8]))
            .into()
    } else {
        text_input("Misc notes", &draft.misc)
            .padding(Padding::from([6, 8]))
            .into()
    };

    let mut lore_col = column![
        text("World lore").size(14),
        text("Link this character to any worlds whose lore should be available during roleplay.").size(12),
    ]
    .spacing(4);

    if worlds.is_empty() {
        lore_col = lore_col.push(
            text("No worlds yet. Create one from the Worlds editor, then return here to link it.").size(12),
        );
    } else {
        let mut worlds_row = row![].spacing(10).align_y(Alignment::Center);
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
        lore_col = lore_col.push(worlds_row);
    }

    let mut gallery_col = column![
        text("Character images").size(14),
        text("Add PNG portraits and describe the expression or scene. Images are top-centered, cropped square, and shown as circles.").size(12),
    ]
    .spacing(4);

    if can_edit {
        gallery_col = gallery_col.push(
            row![
                button(text("Add images…").size(13))
                    .padding(Padding::from([4, 10]))
                    .on_press(CharacterMessage::AddImages),
                button(text("Add folder…").size(13))
                    .padding(Padding::from([4, 10]))
                    .on_press(CharacterMessage::AddImageFolder),
            ]
            .spacing(8),
        );
    }

    for img in &draft.images {
        let img_id = img.id.clone();
        let is_default = draft.default_image_id.as_deref() == Some(&img.id);

        let thumb: Element<'a, CharacterMessage> = if let Some(handle) = image_handle(&img.data) {
            image(handle).width(56).height(56).into()
        } else {
            container(text("No img").size(11))
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
                .padding(Padding::from([4, 8]))
                .into()
        } else {
            text(&img.label).size(13).into()
        };

        let mut img_actions = row![].spacing(6).align_y(Alignment::Center);
        if can_edit {
            let id_for_default = img_id.clone();
            let default_label = if is_default {
                "Default / icon (Active)"
            } else {
                "Default / icon"
            };
            img_actions = img_actions.push(
                button(text(default_label).size(12))
                    .padding(Padding::from([3, 8]))
                    .on_press(CharacterMessage::SetDefaultImage(id_for_default)),
            );
            let id_for_del = img_id.clone();
            img_actions = img_actions.push(
                button(text("Remove").size(12))
                    .padding(Padding::from([3, 8]))
                    .on_press(CharacterMessage::DeleteImage(id_for_del)),
            );
        }

        let img_card = crate::ui::card(
            row![
                thumb,
                column![
                    text("AI label").size(11),
                    label_input,
                    img_actions,
                ]
                .spacing(4)
                .width(Length::Fill),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            light_mode,
            glass_mode,
        );
        gallery_col = gallery_col.push(img_card);
    }

    let right_col = column![
        column![text("Scenario").size(12), scen_input].spacing(2),
        column![text("System prompt").size(12), sys_input].spacing(2),
        column![text("Example dialogue").size(12), ex_input].spacing(2),
        column![text("Misc notes").size(12), misc_input].spacing(2),
        lore_col,
        gallery_col,
    ]
    .spacing(8)
    .width(Length::FillPortion(1));

    let two_columns: Element<'a, CharacterMessage> = if width < 560.0 {
        column![left_col.width(Length::Fill), right_col.width(Length::Fill)]
            .spacing(16)
            .into()
    } else {
        row![left_col, right_col].spacing(16).into()
    };

    let content = column![header_col, actions.wrap(), two_columns]
        .spacing(12)
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
    draft_world_ids: &'a HashSet<String>,
    worlds: &'a [World],
    search_query: &'a str,
    char_import_pending: bool,
    light_mode: bool,
    glass_mode: bool,
    compact: bool,
) -> Element<'a, CharacterMessage> {
    let header = row![
        text("Characters").size(18),
        Space::new().width(Length::Fill),
        button(text("✕").size(14))
            .padding(Padding::from([4, 8]))
            .on_press(CharacterMessage::CloseDialog),
    ]
    .align_y(Alignment::Center)
    .width(Length::Fill);

    let list_pane = character_list_view(
        characters,
        draft.id.as_deref(),
        search_query,
        char_import_pending,
        light_mode,
        glass_mode,
    );

    let editor_pane = character_editor_view(
        draft,
        draft_world_ids,
        worlds,
        light_mode,
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
            container(list_pane).width(Length::Fixed(240.0)).height(Length::Fill),
            container(editor_pane).width(Length::Fill).height(Length::Fill),
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

/// Convenience function to render the character dialog view mapped to an application message type.
#[allow(clippy::too_many_arguments)]
pub fn character_dialog_view_mapped<'a, Message: 'a>(
    characters: &'a [Character],
    draft: &'a DraftCharacter,
    draft_world_ids: &'a HashSet<String>,
    worlds: &'a [World],
    search_query: &'a str,
    char_import_pending: bool,
    light_mode: bool,
    glass_mode: bool,
    compact: bool,
    f: impl Fn(CharacterMessage) -> Message + 'a,
) -> Element<'a, Message> {
    character_dialog_view(
        characters,
        draft,
        draft_world_ids,
        worlds,
        search_query,
        char_import_pending,
        light_mode,
        glass_mode,
        compact,
    )
    .map(f)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn test_draft_character_conversions() {
        let character = Character {
            id: "char-123".into(),
            name: "Lyra".into(),
            description: "A brave warrior".into(),
            personality: "Fierce, loyal".into(),
            scenario: "Ancient ruins".into(),
            system_prompt: "You are Lyra.".into(),
            example_dialogue: "User: Hi\nLyra: Stand ready.".into(),
            appearance: "Armor and spear".into(),
            age: "28".into(),
            gender: "Female".into(),
            race: "Human".into(),
            misc: "Scars from battle".into(),
            tags: vec!["warrior".into(), "human".into()],
            avatar: Some(vec![1, 2, 3]),
            images: vec![CharacterImage {
                id: "img-1".into(),
                label: "combat".into(),
                data: vec![4, 5, 6],
            }],
            default_image_id: Some("img-1".into()),
            is_public: true,
            owned_by_user: true,
            revision: 1,
        };

        let draft = DraftCharacter::from(&character);
        assert_eq!(draft.id.as_deref(), Some("char-123"));
        assert_eq!(draft.name, "Lyra");
        assert_eq!(draft.tags, "warrior, human");
        assert_eq!(draft.images.len(), 1);
        assert!(draft.is_public);
        assert!(draft.owned_by_user);

        // Convert DraftCharacter to CharacterInput
        let input = CharacterInput::from(&draft);
        assert_eq!(input.id.as_deref(), Some("char-123"));
        assert_eq!(input.name, "Lyra");
        assert_eq!(input.tags, vec!["warrior", "human"]);
        // Since images is not empty, avatar should be None according to existing logic
        assert!(input.avatar.is_none());
        assert_eq!(input.images.len(), 1);
        assert!(input.is_public);

        // Convert CharacterInput back to DraftCharacter
        let draft2 = DraftCharacter::from(&input);
        assert_eq!(draft2.name, "Lyra");
        assert_eq!(draft2.tags, "warrior, human");
    }

    #[test]
    fn test_draft_character_update_messages() {
        let mut draft = DraftCharacter::default();

        draft.update(&CharacterMessage::NameChanged("Alice".into()));
        assert_eq!(draft.name, "Alice");

        draft.update(&CharacterMessage::AgeChanged("30".into()));
        assert_eq!(draft.age, "30");

        draft.update(&CharacterMessage::GenderChanged("Female".into()));
        assert_eq!(draft.gender, "Female");

        draft.update(&CharacterMessage::RaceChanged("Elf".into()));
        assert_eq!(draft.race, "Elf");

        draft.update(&CharacterMessage::DescriptionChanged("Mage".into()));
        assert_eq!(draft.description, "Mage");

        draft.update(&CharacterMessage::PersonalityChanged("Calm".into()));
        assert_eq!(draft.personality, "Calm");

        draft.update(&CharacterMessage::ScenarioChanged("Tavern".into()));
        assert_eq!(draft.scenario, "Tavern");

        draft.update(&CharacterMessage::SystemPromptChanged("Be Alice".into()));
        assert_eq!(draft.system_prompt, "Be Alice");

        draft.update(&CharacterMessage::ExampleDialogueChanged("Hello".into()));
        assert_eq!(draft.example_dialogue, "Hello");

        draft.update(&CharacterMessage::AppearanceChanged("Robes".into()));
        assert_eq!(draft.appearance, "Robes");

        draft.update(&CharacterMessage::MiscChanged("Has a staff".into()));
        assert_eq!(draft.misc, "Has a staff");

        draft.update(&CharacterMessage::TagsChanged("magic, spellcaster".into()));
        assert_eq!(draft.tags, "magic, spellcaster");

        draft.update(&CharacterMessage::TogglePublic(true));
        assert!(draft.is_public);

        // Image manipulation
        draft.images.push(CharacterImage {
            id: "i1".into(),
            label: "neutral".into(),
            data: vec![1, 2],
        });
        draft.images.push(CharacterImage {
            id: "i2".into(),
            label: "happy".into(),
            data: vec![3, 4],
        });

        draft.update(&CharacterMessage::SetDefaultImage("i2".into()));
        assert_eq!(draft.default_image_id.as_deref(), Some("i2"));

        draft.update(&CharacterMessage::ImageLabelChanged {
            id: "i1".into(),
            label: "focused".into(),
        });
        assert_eq!(draft.images[0].label, "focused");

        draft.update(&CharacterMessage::DeleteImage("i2".into()));
        assert_eq!(draft.images.len(), 1);
        assert_eq!(draft.default_image_id.as_deref(), Some("i1"));
    }

    #[test]
    fn test_draft_from_card_and_export_roundtrip() {
        let original_draft = DraftCharacter {
            id: None,
            name: "Elysia".into(),
            description: "Adventurous mage".into(),
            personality: "Curious, witty".into(),
            scenario: "Enchanted forest".into(),
            system_prompt: "You are Elysia.".into(),
            example_dialogue: "User: Hi\nElysia: Greetings!".into(),
            appearance: "Green robes, hazel eyes".into(),
            age: "120".into(),
            gender: "Female".into(),
            race: "Elf".into(),
            misc: "Carries a wooden staff".into(),
            tags: "mage, elf, fantasy".into(),
            avatar: None,
            images: vec![],
            default_image_id: None,
            is_public: false,
            owned_by_user: true,
        };

        // Export to SillyTavern JSON v2
        let card = export_card_json(&original_draft);
        assert_eq!(card["spec"], "chara_card_v2");
        assert_eq!(card["spec_version"], "2.0");
        assert_eq!(card["data"]["name"], "Elysia");
        assert_eq!(card["data"]["extensions"]["chatty"]["race"], "Elf");

        // Import back from JSON card
        let imported = draft_from_card(card).expect("should parse valid card");
        assert_eq!(imported.name, original_draft.name);
        assert_eq!(imported.description, original_draft.description);
        assert_eq!(imported.personality, original_draft.personality);
        assert_eq!(imported.scenario, original_draft.scenario);
        assert_eq!(imported.system_prompt, original_draft.system_prompt);
        assert_eq!(imported.example_dialogue, original_draft.example_dialogue);
        assert_eq!(imported.appearance, original_draft.appearance);
        assert_eq!(imported.age, original_draft.age);
        assert_eq!(imported.gender, original_draft.gender);
        assert_eq!(imported.race, original_draft.race);
        assert_eq!(imported.misc, original_draft.misc);
        assert_eq!(imported.tags, original_draft.tags);
    }

    #[test]
    fn test_draft_from_card_fallback_root() {
        // v1 card where fields are at the root
        let root_card = serde_json::json!({
            "name": "Gerald",
            "description": "A blacksmith",
            "personality": "Gruff but kind",
            "scenario": "The forge",
            "mes_example": "Gerald: What do you need forged?",
            "system_prompt": "You are Gerald the blacksmith.",
        });

        let draft = draft_from_card(root_card).expect("should parse root card");
        assert_eq!(draft.name, "Gerald");
        assert_eq!(draft.description, "A blacksmith");
        assert_eq!(draft.personality, "Gruff but kind");
        assert_eq!(draft.scenario, "The forge");
        assert_eq!(draft.example_dialogue, "Gerald: What do you need forged?");
        assert_eq!(draft.system_prompt, "You are Gerald the blacksmith.");
    }

    #[test]
    fn test_png_card_json_parsing() {
        // Invalid PNG: empty buffer
        assert!(png_card_json(&[]).is_none());

        // Invalid PNG: wrong header
        assert!(png_card_json(b"not a png at all").is_none());

        // Valid PNG header only (no chunks)
        assert!(png_card_json(b"\x89PNG\r\n\x1a\n").is_none());

        // Construct a synthetic PNG with a tEXt chara chunk
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");

        // Dummy IHDR chunk (len: 13, type: IHDR, 13 bytes data, 4 bytes crc)
        let ihdr_data = vec![0u8; 13];
        png.extend_from_slice(&(13u32.to_be_bytes()));
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&ihdr_data);
        png.extend_from_slice(&[0, 0, 0, 0]); // CRC

        // Create tEXt chara chunk
        let json_payload = b"{\"name\":\"Hero\",\"personality\":\"Noble\"}";
        let b64_payload = chatty_protocol::util::base64::encode(json_payload);
        let mut text_chunk_data = Vec::new();
        text_chunk_data.extend_from_slice(b"chara\0");
        text_chunk_data.extend_from_slice(b64_payload.as_bytes());

        let chunk_len = text_chunk_data.len() as u32;
        png.extend_from_slice(&chunk_len.to_be_bytes());
        png.extend_from_slice(b"tEXt");
        png.extend_from_slice(&text_chunk_data);
        png.extend_from_slice(&[0, 0, 0, 0]); // CRC

        // Extract with png_card_json
        let extracted = png_card_json(&png).expect("must parse valid tEXt chara chunk");
        assert_eq!(extracted, json_payload);

        // Test non-chara keyword chunk
        let mut png_other_text = Vec::new();
        png_other_text.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        let other_data = b"author\0someone";
        png_other_text.extend_from_slice(&(other_data.len() as u32).to_be_bytes());
        png_other_text.extend_from_slice(b"tEXt");
        png_other_text.extend_from_slice(other_data);
        png_other_text.extend_from_slice(&[0, 0, 0, 0]);
        assert!(png_card_json(&png_other_text).is_none());

        // Test truncated chunk
        let mut truncated = png.clone();
        truncated.truncate(png.len() - 5);
        assert!(png_card_json(&truncated).is_none());
    }

    #[test]
    fn test_image_normalization() {
        use ::image::GenericImageView;
        // Valid square PNG 256x256
        let square = ::image::RgbaImage::new(256, 256);
        let mut square_png = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(square)
            .write_to(&mut square_png, ::image::ImageFormat::Png)
            .unwrap();

        let norm_square = normalize_character_image_png(square_png.get_ref()).unwrap();
        assert_eq!(norm_square, square_png.into_inner());

        // Rectangular PNG 256x512
        let rect = ::image::RgbaImage::new(256, 512);
        let mut rect_png = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(rect)
            .write_to(&mut rect_png, ::image::ImageFormat::Png)
            .unwrap();

        let norm_rect = normalize_character_image_png(rect_png.get_ref()).unwrap();
        let decoded = ::image::load_from_memory_with_format(&norm_rect, ::image::ImageFormat::Png).unwrap();
        assert_eq!(decoded.dimensions(), (256, 256));

        // Too small: 64x64
        let small = ::image::RgbaImage::new(64, 64);
        let mut small_png = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(small)
            .write_to(&mut small_png, ::image::ImageFormat::Png)
            .unwrap();
        assert!(normalize_character_image_png(small_png.get_ref()).is_err());

        // Too large: 600x600 (side > 512)
        let large = ::image::RgbaImage::new(600, 600);
        let mut large_png = std::io::Cursor::new(Vec::new());
        ::image::DynamicImage::ImageRgba8(large)
            .write_to(&mut large_png, ::image::ImageFormat::Png)
            .unwrap();
        assert!(normalize_character_image_png(large_png.get_ref()).is_err());

        // Corrupt data
        assert!(normalize_character_image_png(b"not png").is_err());
    }

    #[test]
    fn test_image_handle_helpers_and_cache() {
        assert!(image_handle(&[]).is_none());
        assert!(image_handle_from_bytes(&[]).is_none());

        let dummy = vec![1, 2, 3, 4];
        let handle = image_handle(&dummy);
        assert!(handle.is_some());

        let mut cache = CharacterImageCache::new();
        assert!(cache.get_or_insert("key1", &[]).is_none());

        let h1 = cache.get_or_insert("key1", &dummy);
        assert!(h1.is_some());
        let h2 = cache.get_or_insert("key1", &dummy);
        assert!(h2.is_some());

        cache.clear();
        assert_eq!(cache.handles.len(), 0);
    }
}
