//! Native Chatty desktop client.

mod admin;
mod app;
mod characters;
mod conversation;
mod memories;
mod network;
mod tts;
mod ui;
mod worlds;

use app::ChattyApp;
use chatty_protocol::util::args::ParsedArgs;
use chatty_protocol::util::{Error, Result, format_err};
use iced::{Color, Size, window};
use std::path::PathBuf;
use std::sync::Arc;
use tts::test_speech;

const USAGE: &str = "chatty-gui -- Chatty client\n\n\
Options:\n\
  --broker <addr>         Broker address [env: CHATTY_BROKER]\n\
  --ca <path>             Pinned CA certificate PEM [env: CHATTY_CA]\n\
  --inspect               Open the inspection harness instead of the app\n\
  --inspect-control <fifo> FIFO for inspection control commands [env: CHATTY_INSPECT_CONTROL]\n\
  --width <px>            Initial window width [default: 1100]\n\
  --height <px>           Initial window height [default: 720]\n\
  --session-file <path>   Session storage path [env: CHATTY_SESSION_FILE]\n\
  --test-speech           Play a short test phrase and exit\n";

#[derive(Clone)]
pub struct Args {
    pub broker: String,
    pub ca: Option<PathBuf>,
    pub inspect: bool,
    pub inspect_control: Option<PathBuf>,
    pub width: f32,
    pub height: f32,
    pub session_file: Option<PathBuf>,
    pub test_speech: bool,
}

fn main() -> Result<()> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| format_err!("failed to install TLS provider"))?;

    let parsed = ParsedArgs::parse(USAGE)?;
    if parsed.flag("test-speech") {
        test_speech().map_err(|e| format_err!("{e}"))?;
        return Ok(());
    }

    let args = Args {
        broker: parsed.string("broker", "CHATTY_BROKER", ""),
        ca: parsed.optional("ca", "CHATTY_CA").map(PathBuf::from),
        inspect: parsed.flag("inspect"),
        inspect_control: parsed
            .optional("inspect-control", "CHATTY_INSPECT_CONTROL")
            .map(PathBuf::from),
        width: parsed.number("width", 1100.0),
        height: parsed.number("height", 720.0),
        session_file: parsed
            .optional("session-file", "CHATTY_SESSION_FILE")
            .map(PathBuf::from),
        test_speech: false,
    };

    let width = args.width;
    let height = args.height;
    let args_arc = Arc::new(args);
    let boot_args = args_arc.clone();

    iced::application(
        move || ChattyApp::boot((*boot_args).clone()),
        ChattyApp::update,
        ChattyApp::view,
    )
    .subscription(ChattyApp::subscription)
    .theme(ChattyApp::theme)
    .style(|_, theme| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: theme.palette().text,
    })
    .scale_factor(ChattyApp::scale_factor)
    .window(window::Settings {
        size: Size::new(width, height),
        min_size: None,
        transparent: true,
        ..Default::default()
    })
    .run()
    .map_err(|e| Error::msg(e.to_string()))
}
