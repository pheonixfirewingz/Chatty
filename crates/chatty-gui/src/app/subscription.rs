//! Event-driven subscriptions: network, speech, admin telemetry, inspection FIFO.

use super::inspect::run_fifo_stream;
use crate::admin::admin_subscription;
use crate::app::{ChattyApp, Message, Screen};
use iced::Subscription;
use iced::futures::StreamExt;
use iced::futures::stream::BoxStream;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub(super) struct ChannelHolder<T>(
    pub(super) Arc<tokio::sync::Mutex<Option<mpsc::UnboundedReceiver<T>>>>,
    pub(super) usize,
);

impl<T> std::hash::Hash for ChannelHolder<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.1.hash(state);
    }
}

impl<T> PartialEq for ChannelHolder<T> {
    fn eq(&self, other: &Self) -> bool {
        self.1 == other.1
    }
}

impl<T> Eq for ChannelHolder<T> {}

pub(super) fn run_channel_stream<T: Send + 'static>(
    holder: &ChannelHolder<T>,
) -> BoxStream<'static, T> {
    let mutex = holder.0.clone();
    iced::stream::channel(
        100,
        move |mut output: iced::futures::channel::mpsc::Sender<T>| async move {
            use iced::futures::sink::SinkExt;
            let rx = {
                let mut guard = mutex.lock().await;
                guard.take()
            };
            if let Some(mut rx) = rx {
                while let Some(item) = rx.recv().await {
                    if output.send(item).await.is_err() {
                        break;
                    }
                }
            }
        },
    )
    .boxed()
}

impl ChattyApp {
    pub(crate) fn subscription(&self) -> Subscription<Message> {
        let net_sub = Subscription::run_with(self.events_holder.clone(), run_channel_stream)
            .map(Message::Network);
        let tts_sub =
            Subscription::run_with(self.tts_holder.clone(), run_channel_stream).map(Message::Tts);
        let admin_sub =
            admin_subscription(self.screen == Screen::Admin && self.admin.admin_tab == 0)
                .map(Message::Admin);

        let fifo_sub = if let Some(path) = &self.fifo_path {
            Subscription::run_with(path.clone(), run_fifo_stream).map(Message::InspectControl)
        } else {
            Subscription::none()
        };

        Subscription::batch(vec![net_sub, tts_sub, admin_sub, fifo_sub])
    }
}
