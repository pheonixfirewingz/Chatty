//! Model library, discovery, and transfers. Each workflow has one home.
use super::{AdminMessage, AdminViewState, format_token_count, hint, human_bytes};
use crate::ui;
use iced::widget::{
    Space, button, column, container, pick_list, progress_bar, row, text, text_input,
};
use iced::{Alignment, Element, Length};

fn action<'a, Message: 'a + Clone + From<AdminMessage>>(
    label: &'a str,
    message: AdminMessage,
    enabled: bool,
) -> iced::widget::Button<'a, Message> {
    button(text(label).size(13))
        .padding([8, 12])
        .style(button::secondary)
        .on_press_maybe(enabled.then(|| Message::from(message)))
}

fn model_label(name: &str) -> &str {
    let name = if name.starts_with("hf.co/") {
        name.split_once(':').map_or(name, |(_, file)| file)
    } else {
        name
    };
    name.strip_suffix(".gguf").unwrap_or(name)
}

pub(super) fn ollama_tab_view<'a, Message: 'a + Clone + From<AdminMessage>>(
    state: &AdminViewState<'a>,
) -> Element<'a, Message> {
    let models = state.models;
    let active = models
        .downloads
        .values()
        .filter(|download| !download.finished)
        .count();
    let downloads = if active == 0 {
        "Downloads".to_owned()
    } else {
        format!("Downloads · {active}")
    };
    let mut navigation = row![].spacing(6);
    for (page, label) in [
        (0, std::borrow::Cow::Borrowed("Installed")),
        (1, std::borrow::Cow::Borrowed("Find models")),
        (3, std::borrow::Cow::Owned(downloads)),
        (2, std::borrow::Cow::Borrowed("Settings")),
    ] {
        let selected = models.page == page;
        navigation = navigation.push(
            button(text(label).size(13))
                .padding([8, 12])
                .style(move |theme, status| {
                    if selected {
                        super::primary_button(theme, status)
                    } else {
                        button::text(theme, status)
                    }
                })
                .on_press(Message::from(AdminMessage::ModelsPage(page))),
        );
    }
    let mut items = column![navigation.wrap()].spacing(12).width(Length::Fill);
    if let Some(notice) = &models.notice {
        items = items.push(text(notice).size(13));
    }
    match models.page {
        2 => return items.push(super::model_settings::view(state)).into(),
        3 => return items.push(downloads_view(state)).into(),
        _ => {}
    }
    let destination_saved =
        models.saved_adapter_url.as_deref() == Some(state.broker_config.adapter_url.as_str());
    if !destination_saved {
        items = items.push(action(
            "Save server changes",
            AdminMessage::ModelsPage(2),
            true,
        ));
    }
    if models.page == 1 {
        items = items.push(
            row![
                text_input("Search Hugging Face", &models.query)
                    .on_input(|value| Message::from(AdminMessage::ModelSearchChanged(value)))
                    .on_submit_maybe(
                        (!models.busy).then(|| Message::from(AdminMessage::SearchModels))
                    )
                    .padding(9)
                    .width(Length::Fill),
                action("Search", AdminMessage::SearchModels, !models.busy)
                    .style(super::primary_button),
            ]
            .spacing(8),
        );
        items = items.push(
            row![
                hint(
                    text_input("Model tag or HF URL", state.ollama_pull_model)
                        .on_input(|value| Message::from(AdminMessage::OllamaPullModelChanged(
                            value
                        )))
                        .padding(9)
                        .width(Length::Fill),
                    "Direct download: an Ollama tag or hf.co/owner/repository:filename.gguf."
                ),
                action(
                    "Download",
                    AdminMessage::DownloadModel(state.ollama_pull_model.trim().into()),
                    destination_saved && models.can_download(state.ollama_pull_model.trim())
                ),
            ]
            .spacing(8),
        );
        for result in &models.results {
            let selected = models.selected_files.get(&result.id).cloned();
            let id = result.id.clone();
            let target = selected
                .as_ref()
                .map(|file| format!("hf.co/{}:{file}", result.id));
            let can_download = target
                .as_deref()
                .is_some_and(|model| models.can_download(model));
            let picker = pick_list(result.files.clone(), selected, move |file| {
                Message::from(AdminMessage::ChooseModelFile {
                    id: id.clone(),
                    file,
                })
            })
            .placeholder("Quantization")
            .text_size(13)
            .width(Length::Fill);
            let card = column![
                row![
                    text(&result.id)
                        .size(15)
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::Glyph),
                    hint(
                        text(format_token_count(result.downloads)).size(12),
                        "Hugging Face downloads"
                    )
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                row![
                    picker,
                    action(
                        "Download",
                        AdminMessage::DownloadModel(target.unwrap_or_default()),
                        destination_saved && can_download
                    )
                    .style(super::primary_button)
                ]
                .spacing(8),
            ]
            .spacing(10);
            items = items.push(ui::card(card, state.light_mode, state.glass_mode));
        }
        return items.into();
    }

    let idle = !models.busy;
    let Some(ollama) = state.ollama_state else {
        return items
            .push(
                row![
                    text("Models unavailable").size(14),
                    action("Refresh", AdminMessage::RefreshOllama, idle)
                ]
                .spacing(12)
                .wrap(),
            )
            .into();
    };
    let disk = ollama.models.iter().map(|model| model.size).sum();
    items = items.push(
        row![
            container(hint(
                text(format!(
                    "{} models · {} loaded · {}",
                    ollama.models.len(),
                    ollama.running_models.len(),
                    human_bytes(disk)
                ))
                .size(13),
                format!(
                    "{} · {}",
                    models.saved_adapter_url.as_deref().unwrap_or(""),
                    ollama.version
                )
            ))
            .width(Length::Fill),
            action("Refresh", AdminMessage::RefreshOllama, idle),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    );
    items = items.push(
        text_input("Filter models", &models.installed_query)
            .on_input(|value| Message::from(AdminMessage::InstalledSearchChanged(value)))
            .padding(9),
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
        let running = ollama
            .running_models
            .iter()
            .find(|running| running.name == model.name);
        let selected = state.broker_config.model == model.name || models.saved_model == model.name;
        let mut title = row![hint(
            text(model_label(&model.name))
                .size(16)
                .wrapping(text::Wrapping::Glyph),
            model.name.as_str()
        )]
        .spacing(8)
        .align_y(Alignment::Center);
        if selected {
            title = title.push(text("Chat model").size(12).color(ui::COLOR_PRIMARY));
        }
        if running.is_some() {
            title = title.push(
                text("Loaded")
                    .size(12)
                    .color(ui::secondary_text_color(state.light_mode)),
            );
        }
        let metadata = [
            human_bytes(model.size),
            model.parameter_size.clone(),
            model.quantization_level.clone(),
        ]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        let details = match running {
            Some(runtime) => format!(
                "{} · VRAM {} · unloads {}",
                model.family,
                human_bytes(runtime.size_vram),
                runtime.expires_at
            ),
            None => model.family.clone(),
        };
        let mut card = column![
            title.wrap(),
            hint(
                text(metadata)
                    .size(13)
                    .color(ui::secondary_text_color(state.light_mode)),
                details
            )
        ]
        .spacing(10)
        .width(Length::Fill);
        let can_delete =
            idle && !selected && running.is_none() && !models.is_downloading(&model.name);
        let delete_help = if models.is_downloading(&model.name) {
            "Cancel this model's download before deleting."
        } else if running.is_some() {
            "Unload this model before deleting."
        } else if selected {
            "Save a different chat model before deleting."
        } else {
            "Delete this model from the inference server."
        };
        let mut controls = row![
            action(
                if selected { "Settings" } else { "Use model" },
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
        ]
        .spacing(8);
        if models.delete_model.as_deref() == Some(&model.name) {
            card = card.push(text(format!("Delete {}?", model_label(&model.name))).size(14));
            controls = controls
                .push(
                    action(
                        "Confirm delete",
                        AdminMessage::OllamaDelete(model.name.clone()),
                        can_delete,
                    )
                    .style(button::danger),
                )
                .push(action("Cancel", AdminMessage::CancelModelDelete, true));
        } else {
            controls = controls.push(hint(
                action(
                    "Delete",
                    AdminMessage::ConfirmModelDelete(model.name.clone()),
                    can_delete,
                ),
                delete_help,
            ));
        }
        items = items.push(ui::card(
            card.push(controls.wrap()),
            state.light_mode,
            state.glass_mode,
        ));
    }
    if count == 0 {
        items = items.push(
            text(if ollama.models.is_empty() {
                "No models"
            } else {
                "No matches"
            })
            .size(14),
        );
        if ollama.models.is_empty() {
            items = items.push(action("Find models", AdminMessage::ModelsPage(1), true));
        }
    }
    items.into()
}

fn downloads_view<'a, Message: 'a + Clone + From<AdminMessage>>(
    state: &AdminViewState<'a>,
) -> Element<'a, Message> {
    let models = state.models;
    let active = models
        .downloads
        .values()
        .filter(|download| !download.finished)
        .count();
    let mut items = column![
        row![
            hint(
                text(format!(
                    "{active}/{} active",
                    chatty_protocol::admin::MAX_PARALLEL_DOWNLOADS
                ))
                .size(13),
                "Downloads run on the inference server. Progress measures the current layer."
            ),
            Space::new().width(Length::Fill),
            hint(
                action(
                    "Clear finished",
                    AdminMessage::ClearFinishedDownloads,
                    models.downloads.values().any(|download| download.finished)
                ),
                "Remove finished entries from this list. Model files stay on the server."
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
    ]
    .spacing(10)
    .width(Length::Fill);
    if models.downloads.is_empty() {
        return items.push(text("No downloads").size(14)).into();
    }
    for (model, download) in &models.downloads {
        let mut title = row![hint(
            text(model_label(model))
                .size(14)
                .wrapping(text::Wrapping::Glyph),
            model.as_str()
        )]
        .spacing(12)
        .align_y(Alignment::Center);
        if !download.finished {
            title = title.push(action(
                if download.cancelling {
                    "Cancelling…"
                } else {
                    "Cancel"
                },
                AdminMessage::CancelDownload(model.clone()),
                !download.cancelling,
            ));
        }
        let mut card = column![title.wrap()].spacing(8).width(Length::Fill);
        let details = format!(
            "{}\n{}\n{}",
            download.destination,
            download.progress.status,
            download.progress.digest.as_deref().unwrap_or("")
        );
        if let (Some(completed), Some(total)) = (
            download.progress.completed,
            download.progress.total.filter(|total| *total > 0),
        ) {
            let fraction = completed.min(total) as f32 / total as f32;
            card = card
                .push(progress_bar(0.0..=1.0, fraction).girth(6))
                .push(hint(
                    text(format!(
                        "{:.1}% · {} / {}",
                        fraction * 100.0,
                        human_bytes(completed.min(total)),
                        human_bytes(total)
                    ))
                    .size(12),
                    details.clone(),
                ));
        }
        if download.progress.total.is_none() || download.finished || download.cancelling {
            card = card.push(hint(
                text(if download.cancelling && !download.finished {
                    "Cancelling…"
                } else {
                    &download.progress.status
                })
                .size(13),
                details,
            ));
        }
        items = items.push(ui::card(card, state.light_mode, state.glass_mode));
    }
    items.into()
}
