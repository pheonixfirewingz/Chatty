//! Admin model discovery and lifecycle workspace. All network access stays in the broker.
use super::{AdminMessage, AdminViewState, format_token_count, human_bytes, inference_config_view};
use crate::ui;
use iced::widget::{Space, button, column, pick_list, row, rule, text, text_input};
use iced::{Alignment, Element, Length};

fn action<'a, Message: 'a + Clone + From<AdminMessage>>(
    label: &'a str,
    message: AdminMessage,
    enabled: bool,
) -> iced::widget::Button<'a, Message> {
    button(text(label).size(14))
        .padding([10, 14])
        .style(|theme, status| {
            let mut style = button::primary(theme, status);
            style.text_color = ui::COLOR_ON_PRIMARY;
            style
        })
        .on_press_maybe(enabled.then(|| Message::from(message)))
}

pub(super) fn ollama_tab_view<'a, Message>(state: &AdminViewState<'a>) -> Element<'a, Message>
where
    Message: 'a + Clone + From<AdminMessage>,
{
    let models = state.models;
    let idle = !models.busy;
    let mut items = column![
        text("Model manager").size(22),
        text(
            "Find GGUF models, download to the inference server, and choose what powers your chats."
        )
        .size(14)
        .color(ui::secondary_text_color(state.light_mode)),
        row![
            action("Installed", AdminMessage::ModelsPage(0), models.page != 0),
            action(
                "Find & download",
                AdminMessage::ModelsPage(1),
                models.page != 1
            ),
            action("Configure", AdminMessage::ModelsPage(2), models.page != 2),
        ]
        .spacing(8)
        .wrap(),
    ]
    .spacing(16)
    .padding([0, 8])
    .width(Length::Fill);

    let destination = models
        .saved_adapter_url
        .as_deref()
        .unwrap_or("Loading saved server configuration…");
    let destination_saved =
        models.saved_adapter_url.as_deref() == Some(state.broker_config.adapter_url.as_str());
    items = items.push(text(format!("Inference server: {destination}")).size(14))
        .push(text("Downloads and model files stay on this inference server. The broker only sends management requests.")
            .size(13).color(ui::secondary_text_color(state.light_mode)));
    if !destination_saved {
        items = items.push(
            text("Save server configuration in Configure before downloading models.").size(14),
        );
    }

    if let Some(notice) = &models.notice {
        items = items.push(ui::card(
            text(notice).size(14),
            state.light_mode,
            state.glass_mode,
        ));
    }
    if models.page == 2 {
        return items.push(text("Chat model & generation settings").size(18))
            .push(text("Settings apply to all chats using this broker. Select a model, adjust settings, then save.").size(14))
            .push(inference_config_view(state))
            .push(action("Save model settings", AdminMessage::SaveBrokerConfig, idle))
            .into();
    }
    if models.page == 1 {
        items = items.push(text("Search Hugging Face").size(18))
            .push(text("Public GGUF models, sorted by downloads. Choose a file to set its quantization. Split and gated files are excluded.").size(14))
            .push(row![
                text_input("Search, e.g. Llama, Qwen, roleplay", &models.query)
                    .on_input(|s| Message::from(AdminMessage::ModelSearchChanged(s)))
                    .on_submit_maybe(idle.then(|| Message::from(AdminMessage::SearchModels)))
                    .padding(10).width(Length::Fill),
                action("Search", AdminMessage::SearchModels, idle),
            ].spacing(8));
        if models.results.is_empty() {
            items = items.push(
                text("Search to find models. No results? Try a broader model name.").size(14),
            );
        }
        for result in &models.results {
            let selected = models.selected_files.get(&result.id).cloned();
            let id = result.id.clone();
            let picker = pick_list(result.files.clone(), selected.clone(), move |file| {
                Message::from(AdminMessage::ChooseModelFile {
                    id: id.clone(),
                    file,
                })
            })
            .placeholder("Choose GGUF file / quantization")
            .width(Length::Fill);
            let target = selected
                .as_ref()
                .map(|file| format!("hf.co/{}:{file}", result.id));
            let card = column![
                text(&result.id).size(16),
                text(format!(
                    "{} downloads · {} file choices",
                    format_token_count(result.downloads),
                    result.files.len()
                ))
                .size(13),
                picker,
                action(
                    "Download selected file",
                    AdminMessage::DownloadModel(target.unwrap_or_default()),
                    idle && destination_saved && selected.is_some()
                ),
            ]
            .spacing(8);
            items = items.push(ui::card(card, state.light_mode, state.glass_mode));
        }
        items = items.push(rule::horizontal(1))
            .push(text("Download by name").size(18))
            .push(text("Use an Ollama tag or hf.co/owner/repository:filename.gguf. Downloads stay on the inference server.").size(14))
            .push(text_input("llama3.2:3b or hf.co/owner/repository:Q4_K_M", state.ollama_pull_model)
                .on_input(|s| Message::from(AdminMessage::OllamaPullModelChanged(s))).padding(10))
            .push(action("Download model", AdminMessage::OllamaPull, idle && destination_saved && !state.ollama_pull_model.trim().is_empty()));
        return items.into();
    }
    items = items.push(
        row![
            text("Installed models").size(18),
            Space::new().width(Length::Fill),
            action("Refresh", AdminMessage::RefreshOllama, idle),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    );
    let Some(ollama) = state.ollama_state else {
        return items.push(text("Connect an Ollama server in Configure, save settings, then Refresh. Other OpenAI-compatible servers support configuration only.").size(14)).into();
    };
    let disk = ollama.models.iter().map(|m| m.size).sum();
    items = items
        .push(
            text(format!(
                "Ollama {} · {} installed · {} loaded · {} on disk",
                ollama.version,
                ollama.models.len(),
                ollama.running_models.len(),
                human_bytes(disk)
            ))
            .size(14),
        )
        .push(
            text_input(
                "Filter installed models by name or family",
                &models.installed_query,
            )
            .on_input(|s| Message::from(AdminMessage::InstalledSearchChanged(s)))
            .padding(10),
        );
    let query = models.installed_query.trim().to_lowercase();
    let mut count = 0;
    for model in &ollama.models {
        if !model.name.to_lowercase().contains(&query)
            && !model.family.to_lowercase().contains(&query)
        {
            continue;
        }
        count += 1;
        let running = ollama.running_models.iter().find(|m| m.name == model.name);
        let selected = state.broker_config.model == model.name || models.saved_model == model.name;
        let mut card = column![
            text(&model.name).size(17),
            text(format!(
                "{} · {} · {} · {}",
                human_bytes(model.size),
                model.family,
                model.parameter_size,
                model.quantization_level
            ))
            .size(14),
            text(format!(
                "{} · {}",
                if running.is_some() {
                    "Loaded in memory"
                } else {
                    "On disk"
                },
                if selected {
                    "Selected chat model"
                } else {
                    "Available"
                }
            ))
            .size(13),
        ]
        .spacing(8)
        .width(Length::Fill);
        if let Some(runtime) = running {
            card = card.push(
                text(format!(
                    "VRAM {} · unloads {}",
                    human_bytes(runtime.size_vram),
                    runtime.expires_at
                ))
                .size(13),
            );
        }
        let controls = row![
            action(
                "Use & configure",
                AdminMessage::ConfigureModel(model.name.clone()),
                idle
            ),
            action(
                if running.is_some() { "Unload" } else { "Load" },
                if running.is_some() {
                    AdminMessage::OllamaUnload(model.name.clone())
                } else {
                    AdminMessage::OllamaLoad(model.name.clone())
                },
                idle
            ),
            action(
                "Delete",
                AdminMessage::ConfirmModelDelete(model.name.clone()),
                idle && !selected && running.is_none()
            ),
        ]
        .spacing(8)
        .wrap();
        card = card.push(controls);
        if selected || running.is_some() {
            card = card.push(
                text("To delete, unload this model and save a different chat model first.")
                    .size(13),
            );
        }
        if models.delete_model.as_deref() == Some(&model.name) {
            card = card.push(text(format!("Delete {} from the inference server? This frees {}. Download again to restore.", model.name, human_bytes(model.size))).size(14))
                .push(row![
                    action("Confirm delete", AdminMessage::OllamaDelete(model.name.clone()), idle && !selected && running.is_none()),
                    action("Cancel", AdminMessage::CancelModelDelete, true),
                ].spacing(8).wrap());
        }
        items = items.push(ui::card(card, state.light_mode, state.glass_mode));
    }
    if count == 0 {
        items = items.push(
            text(if ollama.models.is_empty() {
                "No models installed. Open Find & download to get started."
            } else {
                "No installed models match your filter."
            })
            .size(14),
        );
    }
    items.into()
}
