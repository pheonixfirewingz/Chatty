use super::*;
use chatty_protocol::AdapterStatus;
use iced::Element;

#[test]
fn runtime_adapter_selection_is_saved_and_respects_operator_override() {
    let mut state = AdminState {
        adapter_plugins: vec!["freetoken".into(), "ollama".into()],
        ..AdminState::default()
    };
    for id in ["ollama", "freetoken"] {
        assert!(
            state
                .update(AdminMessage::SelectRuntimeAdapter(id.into()), "admin")
                .is_empty()
        );
        assert_eq!(state.broker_config.adapter_plugin_id(), id);
        let requests = state.update(AdminMessage::SaveBrokerConfig, "admin");
        assert!(
            matches!(&requests[0], Request::AdminSetBrokerConfig { config, .. }
            if config.adapter_plugin == id)
        );
    }
    state.update(
        AdminMessage::SelectRuntimeAdapter("missing".into()),
        "admin",
    );
    assert_eq!(state.broker_config.adapter_plugin, "freetoken");
    state.adapter_override = Some("ollama".into());
    state.update(AdminMessage::SelectRuntimeAdapter("ollama".into()), "admin");
    assert_eq!(state.broker_config.adapter_plugin, "freetoken");
}

#[test]
fn model_settings_navigation_and_disclosure_preserve_saved_values() {
    let mut state = AdminState::default();
    state.broker_config.model = "custom-rp-model".into();
    state.broker_config.num_ctx = 32768;
    state.broker_config.top_k = 20;
    state.broker_config.seed = 42;
    let expected = state.broker_config.clone();
    assert!(
        state
            .update(AdminMessage::ModelsPage(2), "admin")
            .is_empty()
    );
    assert_eq!(state.admin_tab, 2);
    for advanced in [false, true] {
        assert!(
            state
                .update(AdminMessage::ToggleAdvancedSettings(advanced), "admin")
                .is_empty()
        );
        assert_eq!(state.broker_config, expected);
        let _view: Element<'_, AdminMessage> = model_settings::view(&state.as_view_state());
    }
    let saved = state.update(AdminMessage::SaveBrokerConfig, "admin");
    assert!(
        matches!(&saved[..], [Request::AdminSetBrokerConfig { config, .. }] if config == &expected)
    );
    for page in 0..4 {
        state.update(AdminMessage::ModelsPage(page), "admin");
        let _view: Element<'_, AdminMessage> =
            model_manager::ollama_tab_view(&state.as_view_state());
    }
}

#[test]
fn clearing_download_history_keeps_active_and_cancelling_downloads() {
    let mut state = AdminState::default();
    for model in ["finished", "active", "cancelling"] {
        state.update(AdminMessage::DownloadModel(model.into()), "admin");
    }
    state.models.downloads.get_mut("finished").unwrap().finished = true;
    state.models.cancel_download("cancelling");
    assert!(
        state
            .update(AdminMessage::ClearFinishedDownloads, "admin")
            .is_empty()
    );
    assert_eq!(state.models.downloads.len(), 2);
    assert!(state.models.is_downloading("active"));
    assert!(state.models.downloads["cancelling"].cancelling);
}

#[test]
fn test_format_duration() {
    assert_eq!(format_duration(45), "45s");
    assert_eq!(format_duration(125), "2m 5s");
    assert_eq!(format_duration(3665), "1h 1m 5s");
    assert_eq!(format_duration(90065), "1d 1h 1m");
}

#[test]
fn test_human_bytes() {
    assert_eq!(human_bytes(500), "500 B");
    assert_eq!(human_bytes(2048), "2.0 KiB");
    assert_eq!(human_bytes(10 * 1024 * 1024), "10.0 MiB");
    assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
}

#[test]
fn test_format_token_count() {
    assert_eq!(format_token_count(0), "0");
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(1000), "1,000");
    assert_eq!(format_token_count(1234567), "1,234,567");
}

#[test]
fn test_requests_for_tab() {
    let token = "test_tok";

    let r0 = requests_for_tab(0, token);
    assert_eq!(r0.len(), 2);
    assert!(matches!(r0[0], Request::AdminGetBrokerConfig { .. }));
    assert!(matches!(r0[1], Request::AdminGetBrokerMonitor { .. }));

    let r1 = requests_for_tab(1, token);
    assert_eq!(r1.len(), 1);
    assert!(matches!(r1[0], Request::AdminListUsers { .. }));

    let r2 = requests_for_tab(2, token);
    assert_eq!(r2.len(), 2);
    assert!(matches!(r2[0], Request::AdminGetOllamaState { .. }));

    let r3 = requests_for_tab(3, token);
    assert_eq!(r3.len(), 1);
    assert!(matches!(r3[0], Request::AdminGetBrokerConfig { .. }));

    let r4 = requests_for_tab(4, token);
    assert_eq!(r4.len(), 1);
    assert!(matches!(r4[0], Request::AdminReadBrokerLog { .. }));

    let r5 = requests_for_tab(5, token);
    assert_eq!(r5.len(), 1);
    assert!(matches!(r5[0], Request::AdminReadDatabase { .. }));
}

#[test]
fn workflow_files_are_checked_and_summarised() {
    let api = r#"{"1": {"class_type": "CLIPTextEncode", "inputs": {"text": "{{prompt}}"}},
        "2": {"class_type": "KSampler", "inputs": {"seed": "{{seed}}"}}}"#;
    assert!(check_image_workflow(api).is_ok());
    assert!(
        check_image_workflow(r#"{"nodes": [], "links": []}"#)
            .unwrap_err()
            .contains("Export (API)")
    );
    assert!(
        check_image_workflow(r#"{"1": {}}"#)
            .unwrap_err()
            .contains("{{prompt}}")
    );
    assert!(check_image_workflow("{").is_err());
    assert!(workflow_summary("").starts_with("Built-in"));
    assert_eq!(
        workflow_summary(api),
        "Custom workflow: 2 nodes, 1 KiB. Fills {{prompt}}, {{seed}}."
    );
    let mut state = AdminState::default();
    let mut config = state.broker_config.clone();
    config.image.workflow = api.into();
    state.set_broker_config(config);
    assert!(state.image_workflow_summary.contains("2 nodes"));
    state.update(AdminMessage::ClearImageWorkflow, "t");
    assert!(state.broker_config.image.workflow.is_empty());
}

#[test]
fn image_test_uses_default_prompt_and_runs_once() {
    let mut state = AdminState::default();
    let requests = state.update(AdminMessage::TestImageGeneration, "t");
    assert!(matches!(
        requests.as_slice(),
        [Request::GenerateImage { prompt, square: false, .. }] if prompt == DEFAULT_TEST_PROMPT
    ));
    assert!(state.image_test.pending);
    assert!(
        state
            .update(AdminMessage::TestImageGeneration, "t")
            .is_empty()
    );
}

#[test]
fn image_settings_are_saved_with_broker_config() {
    let mut state = AdminState {
        image_plugins: vec!["comfyui".into()],
        image_models: Some(vec!["old.safetensors".into()]),
        ..AdminState::default()
    };
    state.update(AdminMessage::ToggleImages(true), "t");
    state.update(
        AdminMessage::ImageUrlChanged("http://192.168.0.97:8188".into()),
        "t",
    );
    assert!(
        state.image_models.is_none(),
        "a new server needs a new model list"
    );
    state.update(AdminMessage::SelectImagePlugin("unknown".into()), "t");
    state.update(AdminMessage::ImageStepsChanged(30), "t");
    let requests = state.update(AdminMessage::SaveBrokerConfig, "t");
    let [Request::AdminSetBrokerConfig { config, .. }] = requests.as_slice() else {
        panic!("expected a config save")
    };
    assert!(config.image.enabled);
    assert_eq!(config.image.url, "http://192.168.0.97:8188");
    assert_eq!(
        config.image.plugin, "comfyui",
        "unknown plugins are ignored"
    );
    assert_eq!(config.image.steps, 30);
    assert!(matches!(
        state.update(AdminMessage::LoadImageModels, "t").as_slice(),
        [Request::AdminListImageModels { .. }]
    ));
}

#[test]
fn test_admin_state_update() {
    let mut state = AdminState::default();
    let token = "tok123";

    // Tab selection
    let reqs = state.update(AdminMessage::SelectTab(1), token);
    assert_eq!(state.admin_tab, 1);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0], Request::AdminListUsers { .. }));

    // Broker monitor refresh
    let reqs = state.update(AdminMessage::RefreshMonitor, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0], Request::AdminGetBrokerMonitor { .. }));

    // Soft reboot
    let reqs = state.update(AdminMessage::SoftReboot, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0], Request::AdminSoftReboot { .. }));

    // Save config
    let reqs = state.update(AdminMessage::SaveBrokerConfig, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0], Request::AdminSetBrokerConfig { .. }));

    // Adapter controls
    state.update(AdminMessage::ToggleAdapter(true), token);
    assert!(state.broker_config.adapter_enabled);

    state.update(
        AdminMessage::AdapterUrlChanged("http://remote:11434".into()),
        token,
    );
    assert_eq!(state.broker_config.adapter_url, "http://remote:11434");

    state.adapter_plugins = vec!["ollama".into(), "openai-compatible".into()];
    state.update(AdminMessage::SelectRuntimeAdapter("ollama".into()), token);
    assert!(state.broker_config.use_ollama_api);

    state.update(AdminMessage::SelectAdapterModel("llama3.2".into()), token);
    assert_eq!(state.broker_config.model, "llama3.2");

    state.update(AdminMessage::TemperatureChanged(1.2), token);
    assert_eq!(state.broker_config.temperature, 1.2);

    state.update(AdminMessage::TopPChanged(0.85), token);
    assert_eq!(state.broker_config.top_p, 0.85);

    state.update(AdminMessage::TopKChanged(60), token);
    assert_eq!(state.broker_config.top_k, 60);

    state.update(AdminMessage::NumCtxChanged(8192), token);
    assert_eq!(state.broker_config.num_ctx, 8192);

    state.update(AdminMessage::NumPredictChanged(1024), token);
    assert_eq!(state.broker_config.num_predict, 1024);

    state.update(AdminMessage::RepeatPenaltyChanged(1.2), token);
    assert_eq!(state.broker_config.repeat_penalty, 1.2);

    state.update(AdminMessage::SeedChanged(999), token);
    assert_eq!(state.broker_config.seed, 999);

    state.update(AdminMessage::KeepAliveChanged("15m".into()), token);
    assert_eq!(state.broker_config.keep_alive, "15m");

    state.update(
        AdminMessage::EmotionModelChanged("/models/emotion.gguf".into()),
        token,
    );
    assert_eq!(state.broker_config.emotion_model, "/models/emotion.gguf");

    state.update(
        AdminMessage::ReasoningChanged(chatty_protocol::ReasoningLevel::Off),
        token,
    );
    assert_eq!(
        state.broker_config.reasoning,
        chatty_protocol::ReasoningLevel::Off
    );

    state.update(AdminMessage::TogglePublicCharacters(true), token);
    assert!(state.broker_config.allow_public_characters);

    state.update(AdminMessage::ToggleRegistration(false), token);
    assert!(!state.broker_config.allow_self_registration);

    // Ollama actions
    let reqs = state.update(AdminMessage::RefreshOllama, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0], Request::AdminGetOllamaState { .. }));

    state.update(AdminMessage::OllamaPullModelChanged("llama3".into()), token);
    assert_eq!(state.ollama_pull_model, "llama3");

    state.models.busy = false;
    let reqs = state.update(AdminMessage::OllamaPull, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        &reqs[0],
        Request::AdminOllamaAction {
            action: OllamaAction::Pull { model },
            ..
        } if model == "llama3"
    ));

    state.models.busy = false;
    let reqs = state.update(AdminMessage::OllamaLoad("qwen".into()), token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        &reqs[0],
        Request::AdminOllamaAction {
            action: OllamaAction::Load { model },
            ..
        } if model == "qwen"
    ));

    state.models.busy = false;
    let reqs = state.update(AdminMessage::OllamaUnload("qwen".into()), token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        &reqs[0],
        Request::AdminOllamaAction {
            action: OllamaAction::Unload { model },
            ..
        } if model == "qwen"
    ));

    state.models.busy = false;
    state.update(AdminMessage::ConfirmModelDelete("qwen".into()), token);
    let reqs = state.update(AdminMessage::OllamaDelete("qwen".into()), token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        &reqs[0],
        Request::AdminOllamaAction {
            action: OllamaAction::Delete { model },
            ..
        } if model == "qwen"
    ));

    // User management
    state.update(AdminMessage::NewUsernameChanged("bob".into()), token);
    assert_eq!(state.admin_new_username, "bob");

    state.update(AdminMessage::NewPasswordChanged("pwd123".into()), token);
    assert_eq!(state.admin_new_password, "pwd123");

    state.update(AdminMessage::NewRoleChanged(Role::Admin), token);
    assert_eq!(state.admin_new_role, Role::Admin);

    let reqs = state.update(AdminMessage::CreateUser, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        &reqs[0],
        Request::AdminCreateUser {
            username,
            password,
            role: Role::Admin,
            ..
        } if username == "bob" && password == "pwd123"
    ));
    assert!(state.admin_new_password.is_empty());

    let reqs = state.update(
        AdminMessage::SetUserRole {
            user_id: "u2".into(),
            role: Role::User,
        },
        token,
    );
    assert_eq!(reqs.len(), 1);
    assert!(
        matches!(&reqs[0], Request::AdminSetRole { user_id, role: Role::User, .. } if user_id == "u2")
    );

    let reqs = state.update(AdminMessage::DeleteUser("u2".into()), token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        &reqs[0],
        Request::AdminDeleteUser { user_id, .. } if user_id == "u2"
    ));

    // Logs & Data
    let reqs = state.update(AdminMessage::RefreshLogs, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0], Request::AdminReadBrokerLog { .. }));

    state.update(AdminMessage::SearchLogsChanged("warn".into()), token);
    assert_eq!(state.admin_log_search, "warn");

    let reqs = state.update(AdminMessage::RefreshData, token);
    assert_eq!(reqs.len(), 1);
    assert!(matches!(reqs[0], Request::AdminReadDatabase { .. }));

    let reqs = state.update(
        AdminMessage::ToggleCharacterPublic {
            character_id: "char_abc".into(),
            is_public: true,
        },
        token,
    );
    assert_eq!(reqs.len(), 1);
    assert!(matches!(
        &reqs[0],
        Request::AdminSetCharacterPublic {
            character_id,
            is_public: true,
            ..
        } if character_id == "char_abc"
    ));
}

#[test]
#[allow(clippy::field_reassign_with_default)]
fn test_admin_dialog_view_rendering() {
    let mut state = AdminState::default();

    // Sample data for rendering
    state.broker_monitor = Some(BrokerMonitor {
        uptime_seconds: 3661,
        cpu_percent: 12.5,
        memory_used_mb: 256,
        memory_limit_mb: Some(1024),
        active_connections: 3,
        adapter_status: AdapterStatus::Online,
        adapter_model_count: 2,
        adapter_latency_ms: Some(42),
        recent_errors: vec!["Sample error line".into()],
    });

    state.ollama_state = Some(OllamaState {
        version: "0.3.14".into(),
        models: vec![chatty_protocol::OllamaModel {
            name: "llama3.2:latest".into(),
            size: 2_000_000_000,
            modified_at: "now".into(),
            family: "llama".into(),
            parameter_size: "3.2B".into(),
            quantization_level: "Q4_K_M".into(),
        }],
        running_models: vec![chatty_protocol::OllamaRunningModel {
            name: "llama3.2:latest".into(),
            size: 2_000_000_000,
            size_vram: 1_800_000_000,
            expires_at: "in 4 minutes".into(),
        }],
    });

    state.users = vec![UserAccount {
        id: "user-1".into(),
        username: "admin".into(),
        role: Role::Admin,
        created_at: "2025-01-01".into(),
        usage: chatty_protocol::TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 500,
        },
    }];
    state.current_user_id = "user-1".into();

    state.admin_log = BrokerLog {
        content: "2026-10-14 INFO Broker started\n2026-10-14 WARN Connection dropped\n".into(),
        truncated: false,
        file_size_bytes: 84,
    };

    state.admin_data = vec![AdminDataRow {
        kind: "Character".into(),
        id: "c1".into(),
        label: "Lyra".into(),
        detail: "A noble warrior".into(),
        is_public: Some(true),
    }];

    // Test rendering every tab
    for tab in 0..ADMIN_TAB_NAMES.len() {
        state.admin_tab = tab;
        let view_state = state.as_view_state();
        let _element: Element<'_, AdminMessage> = admin_dialog_view(&view_state);
    }
}
#[test]
fn model_download_keeps_file_choice_and_blocks_duplicates() {
    let mut state = AdminState::default();
    let name = "hf.co/owner/chat-GGUF:chat-Q4_K_M.gguf";
    let requests = state.update(AdminMessage::DownloadModel(name.into()), "admin-session");
    assert!(matches!(&requests[..], [Request::AdminOllamaAction {
        session_token, action: OllamaAction::Pull { model }
    }] if session_token == "admin-session" && model == name));
    assert!(!state.models.busy);
    assert!(
        state
            .update(AdminMessage::DownloadModel(name.into()), "admin-session")
            .is_empty()
    );
    assert_eq!(
        state
            .update(AdminMessage::DownloadModel("other".into()), "admin-session")
            .len(),
        1
    );
    assert_eq!(state.models.downloads.len(), 2);
}

#[test]
fn parallel_download_limit_does_not_block_unrelated_deletion() {
    let mut state = AdminState::default();
    for model in ["a", "b", "c", "d"] {
        assert_eq!(
            state
                .update(AdminMessage::DownloadModel(model.into()), "session")
                .len(),
            1
        );
    }
    assert!(
        state
            .update(AdminMessage::DownloadModel("overflow".into()), "session")
            .is_empty()
    );
    state.update(AdminMessage::ConfirmModelDelete("a".into()), "session");
    assert!(
        state
            .update(AdminMessage::OllamaDelete("a".into()), "session")
            .is_empty()
    );
    state.update(
        AdminMessage::ConfirmModelDelete("unrelated".into()),
        "session",
    );
    assert_eq!(
        state
            .update(AdminMessage::OllamaDelete("unrelated".into()), "session")
            .len(),
        1
    );
    state.models.download_started("a", 1);
    state.models.download_for_request(1).unwrap().finished = true;
    assert_eq!(
        state
            .update(AdminMessage::DownloadModel("overflow".into()), "session")
            .len(),
        1
    );
    state.models.page = 1;
    let _view: Element<'_, AdminMessage> =
        super::model_manager::ollama_tab_view(&state.as_view_state());
}

#[test]
fn model_delete_requires_confirmation_and_preserves_saved_model() {
    let mut state = AdminState::default();
    assert!(
        state
            .update(AdminMessage::OllamaDelete("model".into()), "session")
            .is_empty()
    );
    state.update(AdminMessage::ConfirmModelDelete("model".into()), "session");
    state.update(AdminMessage::CancelModelDelete, "session");
    assert!(
        state
            .update(AdminMessage::OllamaDelete("model".into()), "session")
            .is_empty()
    );
    state.models.saved_model = "model".into();
    state.update(
        AdminMessage::ConfigureModel("replacement".into()),
        "session",
    );
    state.update(AdminMessage::ConfirmModelDelete("model".into()), "session");
    assert!(
        state
            .update(AdminMessage::OllamaDelete("model".into()), "session")
            .is_empty()
    );
    state.models.saved_model = "replacement".into();
    assert_eq!(
        state
            .update(AdminMessage::OllamaDelete("model".into()), "session")
            .len(),
        1
    );
}
#[test]
fn model_download_requires_saved_server_changes_and_survives_tab_switches() {
    let mut state = AdminState::default();
    state.models.saved_adapter_url = Some("http://old-server:11434".into());
    state.broker_config.adapter_url = "http://inference-server:11434".into();
    assert!(
        state
            .update(AdminMessage::DownloadModel("llama3.2:3b".into()), "session")
            .is_empty()
    );
    assert!(!state.models.busy);
    state.models.saved_adapter_url = Some(state.broker_config.adapter_url.clone());
    assert_eq!(
        state
            .update(AdminMessage::DownloadModel("llama3.2:3b".into()), "session")
            .len(),
        1
    );
    state.models.download_started("llama3.2:3b", 99);
    state.update(AdminMessage::SelectTab(1), "session");
    assert_eq!(state.update(AdminMessage::SelectTab(2), "session").len(), 2);
    assert_eq!(state.models.downloads["llama3.2:3b"].request_id, Some(99));
    assert!(!state.models.busy);
}

mod image_settings {
    use super::super::image_settings::shares_chat_host;

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
