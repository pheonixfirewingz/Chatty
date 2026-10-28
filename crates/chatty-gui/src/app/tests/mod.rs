//! App-level tests. [`Harness`] owns an app wired to in-memory channels so a
//! test can drive it with messages and broker frames, then inspect what the
//! app asked the network task to send.

mod admin;
mod characters;
mod conversation;
mod inspect;
mod memories;
mod session;
mod view;

use super::{ChattyApp, Message};
use crate::network::{Command, Event};
use chatty_protocol::{ErrorCode, Frame, MessageType, Request, Response, WireError, encode};
use serde::Serialize;
use std::ops::{Deref, DerefMut};
use tokio::sync::mpsc;

pub(super) struct Harness {
    app: ChattyApp,
    sent: mpsc::UnboundedReceiver<Command>,
}

impl Deref for Harness {
    type Target = ChattyApp;
    fn deref(&self) -> &ChattyApp {
        &self.app
    }
}

impl DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut ChattyApp {
        &mut self.app
    }
}

impl Harness {
    /// A fresh, signed-out app.
    pub(super) fn new() -> Self {
        let (commands, sent) = mpsc::unbounded_channel();
        let (_, events) = mpsc::unbounded_channel();
        Self {
            app: ChattyApp::new(commands, events),
            sent,
        }
    }

    /// A fresh app holding a session token.
    pub(super) fn signed_in() -> Self {
        let mut harness = Self::new();
        harness.token = "token".into();
        harness
    }

    /// The inspection demo: one character, one open chat, admin data.
    pub(super) fn demo() -> Self {
        let mut harness = Self::new();
        harness.load_inspection_demo();
        harness
    }

    pub(super) fn update(&mut self, message: impl Into<Message>) {
        let _ = self.app.update(message.into());
    }

    /// Drain every command sent so far.
    pub(super) fn commands(&mut self) -> Vec<Command> {
        std::iter::from_fn(|| self.sent.try_recv().ok()).collect()
    }

    /// Drain every broker request sent so far.
    pub(super) fn requests(&mut self) -> Vec<Request> {
        self.commands()
            .into_iter()
            .filter_map(|command| match command {
                Command::Request(request) => Some(*request),
                _ => None,
            })
            .collect()
    }

    pub(super) fn frame(
        &mut self,
        message_type: MessageType,
        request_id: u64,
        payload: &impl Serialize,
    ) {
        self.app.handle_frame(Frame {
            compressed: false,
            message_type,
            request_id,
            payload: encode(payload).unwrap(),
        });
    }

    pub(super) fn respond(&mut self, request_id: u64, response: &Response) {
        self.frame(MessageType::Response, request_id, response);
    }

    pub(super) fn fail(&mut self, request_id: u64, message: &str) {
        let error = WireError {
            code: ErrorCode::BackendUnavailable,
            message: message.into(),
            retryable: false,
        };
        self.frame(MessageType::Error, request_id, &error);
    }

    pub(super) fn event(&mut self, event: Event) {
        self.app.handle_event(event);
    }

    /// The portrait shown beside the open chat: (name, label, image id, bytes).
    pub(super) fn emotion_portrait(&self) -> Option<(&str, &str, &str, &[u8])> {
        crate::conversation::current_emotion_portrait(
            &self.characters,
            &self.messages,
            self.selected_conversation.as_deref(),
            self.selected_speaker(),
        )
    }
}

/// A solid square PNG.
pub(super) fn png(side: u32) -> Vec<u8> {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(side, side, image::Rgb([90, 60, 40]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    png.into_inner()
}
