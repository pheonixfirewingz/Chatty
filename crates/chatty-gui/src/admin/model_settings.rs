//! Single home for runtime, model, and generation configuration.

use super::{AdminMessage, AdminViewState, hint};
use crate::ui;
use iced::widget::{
    Space, button, column, container, pick_list, row, slider, text, text_input, toggler,
};
use iced::{Alignment, Element, Length};

pub(super) fn field<'a, Message: 'a>(
    label: &'static str,
    help: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![
        container(hint(text(label).size(13), help)).width(Length::Fixed(100.0)),
        container(control).width(Length::Fill),
    ]
    .spacing(12)
    .align_y(Alignment::Center)
    .into()
}

pub(super) fn sampling<'a, Message: 'a + Clone + From<AdminMessage>>(
    label: &'static str,
    help: &'static str,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    change: impl Fn(f32) -> AdminMessage + 'a,
) -> Element<'a, Message> {
    field(
        label,
        help,
        row![
            slider(range, value, move |value| Message::from(change(value)))
                .step(0.01_f32)
                .width(Length::Fill),
            text(format!("{value:.2}"))
                .size(13)
                .width(Length::Fixed(45.0)),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
}

pub(super) fn view<'a, Message: 'a + Clone + From<AdminMessage>>(
    state: &AdminViewState<'a>,
) -> Element<'a, Message> {
    let config = state.broker_config;
    let runtime: Element<'a, Message> = if let Some(id) = state.adapter_override {
        hint(
            text(id).size(14),
            "Set by CHATTY_ADAPTER_PLUGIN on the broker. Remove that override and restart the broker to select another runtime here.",
        )
    } else {
        pick_list(
            state.adapter_plugins.to_vec(),
            Some(config.adapter_plugin_id().to_owned()),
            |id| Message::from(AdminMessage::SelectRuntimeAdapter(id)),
        )
        .width(Length::Fill)
        .into()
    };
    let connection = column![
        row![
            text("Connection").size(16),
            Space::new().width(Length::Fill),
            toggler(config.adapter_enabled).label("Enabled")
                .on_toggle(|value| Message::from(AdminMessage::ToggleAdapter(value))),
        ].spacing(12).align_y(Alignment::Center).wrap(),
        field("Runtime", "Ollama uses its native API. OpenAI-compatible supports generic inference servers.", runtime),
        field("Server", "Inference server address as reached by the broker. Localhost means the broker machine.",
            text_input("http://127.0.0.1:11434", &config.adapter_url)
                .on_input(|value| Message::from(AdminMessage::AdapterUrlChanged(value)))),
        field("Chat model", "Use a model from Installed, or enter a custom model ID. Empty selects the first available model.",
            text_input("Automatic", &config.model)
                .on_input(|value| Message::from(AdminMessage::SelectAdapterModel(value)))),
        field("Emotion classifier", "GGUF path on the broker host, run inside the broker after each reply to pick a portrait. Empty disables it.",
            row![
                text_input("/var/lib/chatty/EmotionTextClassifierLLM.gguf", &config.emotion_model)
                    .on_input(|value| Message::from(AdminMessage::EmotionModelChanged(value))),
                hint(
                    button(text("Download recommended").size(13))
                        .on_press_maybe((!state.emotion_install.pending)
                            .then(|| Message::from(AdminMessage::InstallEmotionModel)))
                        .padding([6, 12])
                        .style(ui::secondary_button_style()),
                    "Downloads JusteLeo/emotion-text-classifier-LLM (720 MB) to the broker's data folder, verifies it, and selects it. No save needed.",
                ),
            ].spacing(8).align_y(Alignment::Center)),
    ].spacing(12);
    let connection = if state.emotion_install.status.is_empty() {
        connection
    } else {
        connection.push(
            text(&state.emotion_install.status)
                .size(12)
                .color(ui::SECONDARY_TEXT),
        )
    };

    let mut generation = column![
        text("Generation").size(16),
        field(
            "Context",
            "Total context window in tokens, including prompts and generated replies.",
            row![
                slider(512..=131072, config.num_ctx, |value| Message::from(
                    AdminMessage::NumCtxChanged(value)
                ))
                .width(Length::Fill),
                text(config.num_ctx.to_string())
                    .size(13)
                    .width(Length::Fixed(62.0)),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
        ),
        field(
            "Reasoning",
            "Hidden thinking before chat replies. Lower levels answer sooner. Models with only on/off thinking treat every level except Off as on.",
            pick_list(
                chatty_protocol::ReasoningLevel::ALL,
                Some(config.reasoning),
                |level| Message::from(AdminMessage::ReasoningChanged(level))
            )
            .width(Length::Fixed(160.0))
        ),
        sampling(
            "Temperature",
            "Higher values increase variation.",
            config.temperature,
            0.0..=2.0,
            AdminMessage::TemperatureChanged
        ),
        field(
            "Reply limit",
            "Maximum generated tokens, including reasoning. -1 uses the provider default.",
            text_input("-1", &config.num_predict.to_string())
                .on_input(|value| Message::from(AdminMessage::NumPredictChanged(
                    value.parse().unwrap_or(-1)
                )))
                .width(Length::Fixed(120.0))
        ),
        toggler(state.models.advanced_settings)
            .label("Advanced")
            .on_toggle(|expanded| Message::from(AdminMessage::ToggleAdvancedSettings(expanded))),
    ]
    .spacing(12);

    if state.models.advanced_settings {
        generation = generation
            .push(sampling("Top P", "Nucleus sampling probability cutoff.", config.top_p, 0.0..=1.0, AdminMessage::TopPChanged))
            .push(field("Top K", "Number of highest-probability token candidates.", row![
                slider(0..=1000, config.top_k, |value| Message::from(AdminMessage::TopKChanged(value))).width(Length::Fill),
                text(config.top_k.to_string()).size(13).width(Length::Fixed(45.0)),
            ].spacing(12).align_y(Alignment::Center)))
            .push(sampling("Repetition", "Repeat penalty. 1.0 disables the penalty.", config.repeat_penalty, 0.5..=2.0, AdminMessage::RepeatPenaltyChanged))
            .push(field("Seed", "-1 selects a random seed.",
                text_input("-1", &config.seed.to_string())
                    .on_input(|value| Message::from(AdminMessage::SeedChanged(value.parse().unwrap_or(-1))))
                    .width(Length::Fixed(120.0))))
            .push(field("Keep alive", "How long Ollama keeps the model loaded. Examples: 5m, 1h, or 0 to unload immediately.",
                text_input("5m", &config.keep_alive)
                    .on_input(|value| Message::from(AdminMessage::KeepAliveChanged(value)))));
    }

    column![
        ui::card(connection, state.glass_mode),
        ui::card(generation, state.glass_mode),
        button(text("Save settings"))
            .style(super::primary_button)
            .padding([9, 16])
            .on_press(Message::from(AdminMessage::SaveBrokerConfig)),
    ]
    .spacing(12)
    .width(Length::Fill)
    .into()
}
