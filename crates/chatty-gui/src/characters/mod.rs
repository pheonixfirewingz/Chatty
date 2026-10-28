//! Character roster, editing views, and SillyTavern card import/export built with Iced 0.14.
//!
//! Provides the data model (`DraftCharacter`), Elm-architecture messages (`CharacterMessage`),
//! Iced view components (`character_dialog_view`, `character_list_view`, `character_editor_view`),
//! image normalization, image handle caching, and SillyTavern JSON/PNG card parsing and export.

mod card;
mod images;
#[cfg(test)]
mod tests;
mod view;

pub use card::*;
pub use images::*;
pub use view::*;

use chatty_protocol::{Character, CharacterImage, CharacterInput};
use iced::widget::text_editor;
#[cfg(test)]
use images::{MAX_CACHED_IMAGE_BYTES, MAX_CACHED_IMAGES};

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
            CharacterMessage::AgeChanged(val) => self.age = val.clone(),
            CharacterMessage::GenderChanged(val) => self.gender = val.clone(),
            CharacterMessage::RaceChanged(val) => self.race = val.clone(),
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

/// Draft fields long enough to need a multi-line editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongField {
    Description,
    Personality,
    Appearance,
    Scenario,
    SystemPrompt,
    ExampleDialogue,
    Misc,
}

impl LongField {
    const ALL: [Self; 7] = [
        Self::Description,
        Self::Personality,
        Self::Appearance,
        Self::Scenario,
        Self::SystemPrompt,
        Self::ExampleDialogue,
        Self::Misc,
    ];

    fn value(self, draft: &DraftCharacter) -> &String {
        match self {
            Self::Description => &draft.description,
            Self::Personality => &draft.personality,
            Self::Appearance => &draft.appearance,
            Self::Scenario => &draft.scenario,
            Self::SystemPrompt => &draft.system_prompt,
            Self::ExampleDialogue => &draft.example_dialogue,
            Self::Misc => &draft.misc,
        }
    }

    /// Tallest the editor grows before it scrolls.
    fn max_height(self) -> f32 {
        match self {
            Self::Description => 240.0,
            Self::ExampleDialogue => 280.0,
            Self::Appearance | Self::Misc => 160.0,
            Self::Personality | Self::Scenario | Self::SystemPrompt => 200.0,
        }
    }

    fn scroll_id(self) -> iced::widget::Id {
        iced::widget::Id::new(match self {
            Self::Description => "character-description",
            Self::Personality => "character-personality",
            Self::Appearance => "character-appearance",
            Self::Scenario => "character-scenario",
            Self::SystemPrompt => "character-system-prompt",
            Self::ExampleDialogue => "character-example-dialogue",
            Self::Misc => "character-misc",
        })
    }

    fn value_mut(self, draft: &mut DraftCharacter) -> &mut String {
        match self {
            Self::Description => &mut draft.description,
            Self::Personality => &mut draft.personality,
            Self::Appearance => &mut draft.appearance,
            Self::Scenario => &mut draft.scenario,
            Self::SystemPrompt => &mut draft.system_prompt,
            Self::ExampleDialogue => &mut draft.example_dialogue,
            Self::Misc => &mut draft.misc,
        }
    }
}

/// Editor buffers for the draft's multi-line fields.
///
/// `text_editor::Content` holds cursor and layout state and is not `Clone`, so it
/// lives beside the draft. Call [`DraftEditors::load`] whenever the draft is replaced.
#[derive(Default)]
pub struct DraftEditors {
    contents: [text_editor::Content; LongField::ALL.len()],
}

impl DraftEditors {
    /// Replace every buffer with the draft's current text.
    pub fn load(&mut self, draft: &DraftCharacter) {
        for field in LongField::ALL {
            self.contents[field as usize] = text_editor::Content::with_text(field.value(draft));
        }
    }

    #[must_use]
    pub fn content(&self, field: LongField) -> &text_editor::Content {
        &self.contents[field as usize]
    }

    /// Apply an editor action and copy edits back into the draft.
    ///
    /// Typing and keyboard movement return a task that scrolls the field's
    /// scrollbar to keep the cursor in view; clicks and drags leave it alone.
    pub fn perform(
        &mut self,
        draft: &mut DraftCharacter,
        field: LongField,
        action: text_editor::Action,
    ) -> iced::Task<CharacterMessage> {
        let content = &mut self.contents[field as usize];
        let follow = matches!(
            action,
            text_editor::Action::Edit(_) | text_editor::Action::Move(_)
        );
        let edit = action.is_edit();
        content.perform(action);
        if edit {
            *field.value_mut(draft) = content.text();
        }
        if !follow {
            return iced::Task::none();
        }
        // The text offset of the cursor approximates its height in the wrapped
        // layout, which the editor does not expose.
        let cursor = content.cursor().position;
        let (mut before, mut total) = (0, 0);
        for index in 0..content.line_count() {
            let len = content.line(index).map_or(0, |line| line.text.len()) + 1;
            if index < cursor.line {
                before += len;
            }
            total += len;
        }
        let y = (before + cursor.column) as f32 / total.saturating_sub(1).max(1) as f32;
        iced::widget::operation::snap_to(
            field.scroll_id(),
            iced::widget::scrollable::RelativeOffset {
                x: None,
                y: Some(y.min(1.0)),
            },
        )
    }
}

/// User actions within the character dialog, roster, and character editor.
#[derive(Debug, Clone, PartialEq)]
pub enum CharacterMessage {
    // Form field updates
    NameChanged(String),
    /// Cursor, selection, or edit in a multi-line field; see [`DraftEditors`].
    Edit(LongField, text_editor::Action),
    AgeChanged(String),
    GenderChanged(String),
    RaceChanged(String),
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
    /// Ask the broker's image service for a portrait from the draft's details.
    GeneratePortrait,
    /// Generate one portrait per missing emotion in [`EMOTIONS`], with one seed.
    GenerateEmotions,
    /// Stop a running portrait or emotion generation.
    CancelImageGeneration,
    AddImageFolder,
    SetDefaultImage(String),
    DeleteImage(String),
    ImageLabelChanged {
        id: String,
        label: String,
    },

    // Navigation
    Chat(String),
    OpenMemories(String),

    // Lorebook / World linking
    ToggleWorldLink {
        world_id: String,
        linked: bool,
    },

    // Search filter
    SearchChanged(String),
}

/// Whether a view can request a generated image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageGeneration {
    Unavailable,
    Ready,
    Pending,
    /// An emotion set is generating; `done` of `total` images have arrived.
    Batch {
        done: usize,
        total: usize,
    },
}

/// Emotion labels and the expression text given to the image model. Labels are
/// what the reply-time portrait selector matches against. Expressions are
/// deliberately explicit: fast distilled models follow subtle wording weakly.
/// Avoid colour words, which leak into clothing and backgrounds.
pub const EMOTIONS: [(&str, &str); 12] = [
    ("neutral", "neutral calm expression"),
    ("happy", "happy, warm smile"),
    ("laughing", "laughing, eyes crinkled with joy"),
    (
        "sad",
        "sad tearful face, teary eyes, trembling lips, eyebrows drawn together",
    ),
    ("angry", "angry, furrowed brows, clenched jaw"),
    (
        "surprised",
        "shocked surprised face, eyebrows raised high, eyes wide open, mouth open in a gasp",
    ),
    (
        "scared",
        "terrified frightened face, eyes wide with fear, eyebrows pulled up, mouth open",
    ),
    (
        "embarrassed",
        "embarrassed flustered face, blushing cheeks, shy awkward smile, eyes looking away",
    ),
    ("flirty", "flirty, playful smile, half-lidded eyes"),
    ("thoughtful", "thoughtful, pensive gaze"),
    ("smug", "smug, confident smirk"),
    (
        "disgusted",
        "disgusted grossed-out face, nose wrinkled, upper lip curled in revulsion",
    ),
];

/// Character limits enforced by the broker on save.
pub const MAX_CHARACTER_IMAGES: usize = 32;
pub const MAX_CHARACTER_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Emotion sets use a smaller side so a full set fits the byte budget.
pub const EMOTION_PORTRAIT_SIDE: u32 = 384;
pub const PORTRAIT_SIDE: u32 = 512;

/// Emotions in [`EMOTIONS`] without an image of that label in the draft.
#[must_use]
pub fn missing_emotions(draft: &DraftCharacter) -> Vec<(&'static str, &'static str)> {
    EMOTIONS
        .into_iter()
        .filter(|(label, _)| {
            !draft
                .images
                .iter()
                .any(|image| image.label.trim().eq_ignore_ascii_case(label))
        })
        .collect()
}

/// Builds an image prompt from the visible details of a character draft. The
/// expression comes first so models weight it strongly.
#[must_use]
pub fn portrait_prompt(draft: &DraftCharacter, expression: Option<&str>) -> String {
    let details = [&draft.age, &draft.gender, &draft.race, &draft.appearance]
        .into_iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    let expression = expression
        .map(|value| format!("{value}, "))
        .unwrap_or_default();
    let prompt = format!(
        "character portrait, {expression}head and shoulders, looking at viewer, detailed face, {details}"
    );
    prompt.chars().take(2000).collect()
}
