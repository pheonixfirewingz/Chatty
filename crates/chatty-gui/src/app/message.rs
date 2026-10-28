//! The top-level message type and conversions from feature messages.

use crate::admin::AdminMessage;
use crate::characters::CharacterMessage;
use crate::conversation::ConversationMessage;
use crate::memories::MemoryMessage;
use crate::network::Event;
use crate::tts::TtsEvent;
use crate::worlds::WorldMessage;

#[derive(Debug, Clone)]
pub(crate) enum Message {
    Network(Event),
    Tts(TtsEvent),
    Admin(AdminMessage),
    Character(CharacterMessage),
    World(WorldMessage),
    Memory(MemoryMessage),
    Conversation(ConversationMessage),
    InspectControl(String),
    ScreenshotCaptured(iced::window::Screenshot),
    ServerAddressChanged(String),
    TryConnect,
    LoginUsernameChanged(String),
    LoginPasswordChanged(String),
    SubmitLogin,
    SubmitRegister,
    ChangeServer,
    SelectConversation(String),
    DeleteConversation(String),
    HoverConversation(Option<String>),
    OpenNewChat,
    OpenCharactersDialog,
    CloseCharactersDialog,
    OpenWorldsDialog,
    CloseWorldsDialog,
    OpenMemoriesDialog(Option<String>, Option<String>),
    CloseMemoriesDialog,
    OpenAdminDialog,
    CloseAdminDialog,
    OpenSettingsDialog,
    CloseSettingsDialog,
    SetTtsAutoSpeak(bool),
    SignOut,
    ToggleSidebar,
    ToggleCompactSidebar,
    TrustCertificate(bool),
    DismissError,
}

impl From<AdminMessage> for Message {
    fn from(msg: AdminMessage) -> Self {
        Message::Admin(msg)
    }
}

impl From<WorldMessage> for Message {
    fn from(msg: WorldMessage) -> Self {
        Message::World(msg)
    }
}

impl From<MemoryMessage> for Message {
    fn from(msg: MemoryMessage) -> Self {
        Message::Memory(msg)
    }
}

impl From<CharacterMessage> for Message {
    fn from(msg: CharacterMessage) -> Self {
        Message::Character(msg)
    }
}

impl From<ConversationMessage> for Message {
    fn from(msg: ConversationMessage) -> Self {
        Message::Conversation(msg)
    }
}
