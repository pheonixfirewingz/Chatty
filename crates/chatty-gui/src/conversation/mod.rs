//! Chat transcript, composer, and new-chat dialog.

mod context_usage;
#[cfg(test)]
mod tests;
mod view;

pub use view::*;

use crate::characters::ImageGeneration;
use chatty_protocol::conversation::ChatMessage;
use chatty_protocol::response::GenerationStatus;
use chatty_protocol::{Character, GenerationContext};
use std::collections::HashMap;

/// Interaction messages for the conversation view and new-chat dialog.
#[derive(Debug, Clone, PartialEq)]
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
    /// Copy text to the system clipboard.
    CopyText(String),
    /// Illustrate the scene; a non-empty composer input is used as the prompt.
    GenerateSceneImage,
    /// Cancel the pending scene image.
    CancelSceneImage,
}

/// Keeps parsed data only for loaded messages and the current stream.
#[derive(Debug, Clone, Default)]
pub struct ConversationMarkdown {
    messages: Vec<crate::ui::Markdown>,
    stream: crate::ui::Markdown,
}

impl ConversationMarkdown {
    pub fn update(&mut self, messages: &[ChatMessage], stream: &str) {
        self.messages.truncate(messages.len());
        self.messages.resize_with(messages.len(), Default::default);
        for (parsed, message) in self.messages.iter_mut().zip(messages) {
            parsed.update(displayed_message_content(message));
        }
        self.stream.update(stream);
    }
}

/// Zero-copy snapshot of application state required to render the conversation view.
#[derive(Debug, Clone)]
pub struct ConversationViewState<'a> {
    pub characters: &'a [Character],
    pub messages: &'a [ChatMessage],
    pub markdown: &'a ConversationMarkdown,
    pub selected_conversation: Option<&'a str>,
    pub selected_speaker: Option<&'a str>,
    pub input: &'a str,
    pub active_request: Option<u64>,
    pub stream_text: &'a str,
    pub typing_character: Option<&'a str>,
    pub generation_status: Option<GenerationStatus>,
    pub context_usage: Option<&'a GenerationContext>,
    pub editing_message_id: Option<&'a str>,
    pub editing_message_content: &'a str,
    pub hovered_message_id: Option<&'a str>,
    pub sidebar_visible: bool,
    pub available_size: iced::Size,
    pub glass_mode: bool,
    /// Loaded PNG bytes for recent image messages, keyed by message ID.
    pub message_images: &'a HashMap<String, Vec<u8>>,
    pub scene_image: ImageGeneration,
}

/// Zero-copy snapshot of application state required to render the new-chat modal dialog.
#[derive(Debug, Clone)]
pub struct NewChatViewState<'a> {
    pub is_open: bool,
    pub characters: &'a [Character],
    pub selected_character_id: Option<&'a str>,
    pub title: &'a str,
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
pub fn current_emotion_portrait<'a>(
    characters: &'a [Character],
    messages: &[ChatMessage],
    selected_conversation: Option<&str>,
    selected_speaker: Option<&str>,
) -> Option<(&'a str, &'a str, &'a str, &'a [u8])> {
    selected_conversation?;
    for message in messages.iter().rev() {
        let (Some(character_id), Some(image_id)) = (
            message.author_id.as_deref(),
            message.character_image_id.as_deref(),
        ) else {
            continue;
        };
        let Some(character) = characters.iter().find(|c| c.id == character_id) else {
            continue;
        };
        if let Some(image) = character.images.iter().find(|img| img.id == image_id) {
            return Some((&character.name, &image.label, &image.id, &image.data));
        }
    }

    let character_id = selected_speaker?;
    let character = characters.iter().find(|c| c.id == character_id)?;
    let image_id = character.default_image_id.as_deref()?;
    let image = character.images.iter().find(|img| img.id == image_id)?;
    Some((&character.name, &image.label, &image.id, &image.data))
}
