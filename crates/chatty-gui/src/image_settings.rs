//! Admin portal tab for the text-to-image service.
use super::model_settings::{field, sampling};
use super::{AdminMessage, AdminViewState};
use crate::ui;
use iced::widget::{
    Space, button, column, container, pick_list, row, slider, text, text_input, toggler,
};
use iced::{Alignment, Element, Length};

/// Host part of a URL, without scheme, credentials, port, or path.
fn host(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(ipv6) = authority.strip_prefix('[') {
        ipv6.split(']').next()?
    } else {
        authority.split(':').next()?
    }
    .to_ascii_lowercase();
    Some(match host.as_str() {
        "localhost" | "127.0.0.1" | "::1" => "loopback".into(),
        "" => return None,
        _ => host,
    })
}

/// Mirrors the broker's shared-host check, which serializes text and image models.
fn shares_chat_host(chat_url: &str, image_url: &str) -> bool {
    matches!((host(chat_url), host(image_url)), (Some(a), Some(b)) if a == b)
}

pub(super) fn view<'a, Message: 'a + Clone + From<AdminMessage>>(
    state: &AdminViewState<'a>,
) -> Element<'a, Message> {
    let image = &state.broker_config.image;
    let model: Element<'a, Message> = match state.image_models {
        Some(models) if !models.is_empty() => pick_list(
            models.to_vec(),
            Some(image.model.clone()).filter(|model| !model.is_empty()),
            |model| Message::from(AdminMessage::ImageModelChanged(model)),
        )
        .placeholder("Automatic")
        .width(Length::Fill)
        .into(),
        _ => text_input("Automatic", &image.model)
            .on_input(|value| Message::from(AdminMessage::ImageModelChanged(value)))
            .into(),
    };
    let listed = match state.image_models {
        None => "Save the server, then list its models to check the connection.",
        Some([]) => "Connected, but the server has no checkpoints installed.",
        Some(_) => "Connected. Empty selects the first listed model.",
    };
    let side = |value: u32, change: fn(u32) -> AdminMessage| {
        row![
            slider(256..=2048, value, move |value| Message::from(change(
                value / 8 * 8
            )))
            .step(64u32)
            .width(Length::Fill),
            text(value.to_string()).size(13).width(Length::Fixed(45.0)),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
    };
    let connection = column![
        row![
            text("Image service").size(16),
            Space::new().width(Length::Fill),
            toggler(image.enabled)
                .label("Enabled")
                .on_toggle(|value| Message::from(AdminMessage::ToggleImages(value))),
        ]
        .spacing(12)
        .align_y(Alignment::Center)
        .wrap(),
        text("Generates character portraits and chat scene images. Separate from the chat model.")
            .size(12)
            .color(ui::SECONDARY_TEXT),
        field(
            "Service",
            "Image plugin. ComfyUI runs a single-checkpoint text-to-image workflow.",
            pick_list(
                state.image_plugins.to_vec(),
                Some(image.plugin.clone()),
                |id| { Message::from(AdminMessage::SelectImagePlugin(id)) }
            )
            .width(Length::Fill)
        ),
        field(
            "Server",
            "Image server address as reached by the broker, such as http://192.168.0.97:8188.",
            text_input("http://127.0.0.1:8188", &image.url)
                .on_input(|value| Message::from(AdminMessage::ImageUrlChanged(value)))
        ),
        field(
            "Model",
            "All-in-one checkpoint on the image server.",
            row![
                container(model).width(Length::Fill),
                button(text("List models").size(13))
                    .on_press(Message::from(AdminMessage::LoadImageModels)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        ),
        text(listed).size(12).color(ui::SECONDARY_TEXT),
    ]
    .spacing(12);
    let connection = if shares_chat_host(&state.broker_config.adapter_url, &image.url) {
        connection.push(
            text("Same host as the chat model. The broker runs one kind at a time and unloads the other model before switching, so the first request after a switch is slower.")
                .size(12)
                .color(ui::ACCENT_TEXT),
        )
    } else {
        connection
    };
    let generation = column![
        text("Generation").size(16),
        field(
            "Width",
            "Scene image width. Portraits use the shorter side for a square image.",
            side(image.width, AdminMessage::ImageWidthChanged)
        ),
        field(
            "Height",
            "Scene image height.",
            side(image.height, AdminMessage::ImageHeightChanged)
        ),
        field(
            "Steps",
            "Sampling steps. More steps are slower.",
            row![
                slider(1..=150, image.steps, |value| Message::from(
                    AdminMessage::ImageStepsChanged(value)
                ))
                .width(Length::Fill),
                text(image.steps.to_string())
                    .size(13)
                    .width(Length::Fixed(45.0)),
            ]
            .spacing(12)
            .align_y(Alignment::Center)
        ),
        sampling(
            "Guidance",
            "Classifier-free guidance (CFG) scale. Distilled models often need 1.",
            image.guidance,
            0.0..=30.0,
            AdminMessage::ImageGuidanceChanged
        ),
        field(
            "Avoid",
            "Negative prompt appended to every image request.",
            text_input("blurry, low quality", &image.negative_prompt)
                .on_input(|value| Message::from(AdminMessage::ImageNegativeChanged(value)))
        ),
    ]
    .spacing(12);
    let secondary = ui::SECONDARY_TEXT;
    let mut workflow = column![
        text("Workflow").size(16),
        text(state.image_workflow_summary).size(13),
        row![
            button(text("Load workflow file…").size(13))
                .on_press(Message::from(AdminMessage::LoadImageWorkflow)),
            button(text("Use built-in").size(13)).on_press_maybe(
                (!image.workflow.is_empty())
                    .then(|| Message::from(AdminMessage::ClearImageWorkflow))
            ),
        ]
        .spacing(8)
        .wrap(),
        text(
            "Build any flow in ComfyUI (LoRAs, upscalers, face detailers, split model loaders), then use Workflow > Export (API). \
             Put placeholders in node inputs: {{prompt}} {{negative_prompt}} {{model}} {{seed}} {{width}} {{height}} {{steps}} {{guidance}}. \
             A value that is only a placeholder becomes a number where needed. Chatty uses the first output image; keep one image output node."
        )
        .size(12)
        .color(secondary),
    ]
    .spacing(10);
    if let Some(notice) = state.image_notice {
        workflow = workflow.push(text(notice).size(12).color(ui::ACCENT_TEXT));
    }

    let test = state.image_test;
    let mut test_card = column![
        text("Test").size(16),
        text("Renders one image with the saved settings. Save changes first.")
            .size(12)
            .color(secondary),
        row![
            text_input(super::DEFAULT_TEST_PROMPT, &test.prompt)
                .on_input(|value| Message::from(AdminMessage::ImageTestPromptChanged(value)))
                .on_submit(Message::from(AdminMessage::TestImageGeneration))
                .width(Length::Fill),
            button(
                text(if test.pending {
                    "Generating…"
                } else {
                    "Generate test image"
                })
                .size(13)
            )
            .on_press_maybe(
                (!test.pending).then(|| Message::from(AdminMessage::TestImageGeneration))
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(10);
    if let Some(notice) = &test.notice {
        test_card = test_card.push(text(notice).size(12).color(ui::ACCENT_TEXT));
    }
    if let Some(handle) = test
        .result
        .as_deref()
        .and_then(crate::characters::image_handle)
    {
        if let Some(elapsed) = test.elapsed {
            test_card = test_card.push(
                text(format!("Generated in {:.1} s", elapsed.as_secs_f32()))
                    .size(12)
                    .color(secondary),
            );
        }
        test_card = test_card.push(
            container(iced::widget::image(handle).content_fit(iced::ContentFit::Contain))
                .max_width(512),
        );
    }

    column![
        ui::card(connection, state.glass_mode),
        ui::card(generation, state.glass_mode),
        ui::card(workflow, state.glass_mode),
        button(text("Save image settings"))
            .style(super::primary_button)
            .padding([9, 16])
            .on_press(Message::from(AdminMessage::SaveBrokerConfig)),
        ui::card(test_card, state.glass_mode),
    ]
    .spacing(12)
    .width(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::shares_chat_host;

    #[test]
    fn shared_host_ignores_scheme_port_and_path() {
        assert!(shares_chat_host(
            "http://192.168.0.97:11434/v1",
            "http://192.168.0.97:8188"
        ));
        assert!(shares_chat_host(
            "http://localhost:11434",
            "https://127.0.0.1:8188/"
        ));
        assert!(shares_chat_host(
            "http://[::1]:11434",
            "http://localhost:8188"
        ));
        assert!(!shares_chat_host(
            "http://192.168.0.97:11434",
            "http://192.168.0.98:8188"
        ));
        assert!(!shares_chat_host("", ""));
    }
}
