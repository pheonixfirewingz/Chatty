use super::*;
use chatty_protocol::Character;
use iced::Element;

#[test]
fn test_world_message_variants() {
    let msg = WorldMessage::SelectWorld(Some("w1".into()));
    assert_eq!(msg, WorldMessage::SelectWorld(Some("w1".into())));

    let msg_new = WorldMessage::NewWorld;
    assert_eq!(msg_new, WorldMessage::NewWorld);

    let msg_save = WorldMessage::SaveWorld;
    assert_eq!(msg_save, WorldMessage::SaveWorld);

    let msg_del = WorldMessage::DeleteWorld;
    assert_eq!(msg_del, WorldMessage::DeleteWorld);

    let msg_name = WorldMessage::NameChanged("New Name".into());
    assert_eq!(msg_name, WorldMessage::NameChanged("New Name".into()));

    let msg_link = WorldMessage::ToggleCharacterLink {
        character_id: "c1".into(),
        linked: true,
    };
    assert_eq!(
        msg_link,
        WorldMessage::ToggleCharacterLink {
            character_id: "c1".into(),
            linked: true,
        }
    );

    let msg_add = WorldMessage::AddEntry;
    assert_eq!(msg_add, WorldMessage::AddEntry);

    let msg_remove = WorldMessage::RemoveEntry(2);
    assert_eq!(msg_remove, WorldMessage::RemoveEntry(2));

    let msg_search = WorldMessage::SearchChanged("term".into());
    assert_eq!(msg_search, WorldMessage::SearchChanged("term".into()));

    let msg_clear = WorldMessage::ClearSearch;
    assert_eq!(msg_clear, WorldMessage::ClearSearch);

    let msg_enabled = WorldMessage::EntryEnabledToggled {
        index: 0,
        enabled: true,
    };
    assert_eq!(
        msg_enabled,
        WorldMessage::EntryEnabledToggled {
            index: 0,
            enabled: true,
        }
    );

    let msg_prio = WorldMessage::EntryPriorityChanged {
        index: 1,
        priority: 42,
    };
    assert_eq!(
        msg_prio,
        WorldMessage::EntryPriorityChanged {
            index: 1,
            priority: 42,
        }
    );

    let msg_import = WorldMessage::ImportLorebook;
    assert_eq!(msg_import, WorldMessage::ImportLorebook);

    let msg_close = WorldMessage::Close;
    assert_eq!(msg_close, WorldMessage::Close);
}

#[test]
fn test_world_dialog_state_selection_and_crud() {
    let mut state = WorldDialogState::default();
    let existing = World {
        id: "world-1".into(),
        name: "Fantasy Realm".into(),
        character_ids: vec!["char-1".into()],
        entries: vec![WorldFact {
            title: "Magic".into(),
            content: "Everywhere".into(),
            keywords: vec!["spell".into()],
            common_knowledge: true,
            enabled: true,
            priority: 10,
        }],
    };
    let worlds = vec![existing.clone()];

    // Select world
    let action = state.update(&worlds, WorldMessage::SelectWorld(Some("world-1".into())));
    assert_eq!(action, WorldAction::None);
    assert_eq!(state.world_draft.name, "Fantasy Realm");

    // Name change
    state.update(&worlds, WorldMessage::NameChanged("Updated Realm".into()));
    assert_eq!(state.world_draft.name, "Updated Realm");

    // Toggle character
    state.update(
        &worlds,
        WorldMessage::ToggleCharacterLink {
            character_id: "char-2".into(),
            linked: true,
        },
    );
    assert!(
        state
            .world_draft
            .character_ids
            .contains(&"char-2".to_string())
    );

    state.update(
        &worlds,
        WorldMessage::ToggleCharacterLink {
            character_id: "char-2".into(),
            linked: false,
        },
    );
    assert!(
        !state
            .world_draft
            .character_ids
            .contains(&"char-2".to_string())
    );

    // Save world
    let save_action = state.update(&worlds, WorldMessage::SaveWorld);
    match save_action {
        WorldAction::Save(w) => assert_eq!(w.name, "Updated Realm"),
        _ => panic!("Expected WorldAction::Save"),
    }

    // Delete world
    let del_action = state.update(&worlds, WorldMessage::DeleteWorld);
    assert_eq!(del_action, WorldAction::Delete("world-1".into()));
    assert_eq!(state.world_draft, World::default());
}

#[test]
fn test_world_entries_management() {
    let mut state = WorldDialogState::default();
    state.world_draft.name = "Test".into();

    // Add entry
    state.update(&[], WorldMessage::AddEntry);
    assert_eq!(state.world_draft.entries.len(), 1);
    assert!(state.world_draft.entries[0].enabled);

    // Edit entry fields
    state.update(
        &[],
        WorldMessage::EntryTitleChanged {
            index: 0,
            title: "Secret Cave".into(),
        },
    );
    state.update(
        &[],
        WorldMessage::EntryContentChanged {
            index: 0,
            content: "Hidden entrance behind waterfall".into(),
        },
    );
    state.update(
        &[],
        WorldMessage::EntryKeywordsChanged {
            index: 0,
            keywords: "cave, waterfall, hidden".into(),
        },
    );
    state.update(
        &[],
        WorldMessage::EntryPriorityChanged {
            index: 0,
            priority: 5,
        },
    );
    state.update(
        &[],
        WorldMessage::EntryCommonKnowledgeToggled {
            index: 0,
            common_knowledge: false,
        },
    );
    state.update(
        &[],
        WorldMessage::EntryEnabledToggled {
            index: 0,
            enabled: true,
        },
    );

    let entry = &state.world_draft.entries[0];
    assert_eq!(entry.title, "Secret Cave");
    assert_eq!(entry.content, "Hidden entrance behind waterfall");
    assert_eq!(entry.keywords, vec!["cave", "waterfall", "hidden"]);
    assert_eq!(entry.priority, 5);
    assert!(!entry.common_knowledge);
    assert!(entry.enabled);

    // Search & filter
    state.update(&[], WorldMessage::SearchChanged("waterfall".into()));
    assert_eq!(state.entry_search, "waterfall");

    let matches = filter_entries(&state.world_draft.entries, &state.entry_search);
    assert_eq!(matches.len(), 1);

    state.update(&[], WorldMessage::SearchChanged("nonexistent".into()));
    let no_matches = filter_entries(&state.world_draft.entries, &state.entry_search);
    assert_eq!(no_matches.len(), 0);

    state.update(&[], WorldMessage::ClearSearch);
    assert_eq!(state.entry_search, "");

    // Remove entry
    state.update(&[], WorldMessage::RemoveEntry(0));
    assert_eq!(state.world_draft.entries.len(), 0);
}

#[test]
fn test_parse_silly_tavern_lorebook() {
    let valid_json = br#"{"entries": {"0": {"key": ["forest"], "comment": "The dark woods"}}}"#;
    let parsed = parse_silly_tavern_lorebook(valid_json);
    assert!(parsed.is_ok());

    let invalid_json = b"not a json";
    assert!(parse_silly_tavern_lorebook(invalid_json).is_err());

    let too_large = vec![b'a'; 3 * 1024 * 1024];
    assert!(parse_silly_tavern_lorebook(&too_large).is_err());
}

#[test]
fn test_world_dialog_view_creation() {
    let worlds = vec![World {
        id: "w1".into(),
        name: "Kingdom".into(),
        character_ids: vec![],
        entries: vec![WorldFact {
            title: "King".into(),
            content: "Reigns wisely".into(),
            keywords: vec![],
            common_knowledge: true,
            enabled: true,
            priority: 0,
        }],
    }];
    let characters = vec![Character {
        id: "c1".into(),
        name: "Arthur".into(),
        description: String::new(),
        personality: String::new(),
        scenario: String::new(),
        system_prompt: String::new(),
        example_dialogue: String::new(),
        appearance: String::new(),
        age: String::new(),
        gender: String::new(),
        race: String::new(),
        misc: String::new(),
        tags: Vec::new(),
        avatar: None,
        images: Vec::new(),
        default_image_id: None,
        is_public: false,
        owned_by_user: true,
        revision: 0,
    }];
    let state = WorldViewState {
        worlds: &worlds,
        world_draft: &worlds[0],
        characters: &characters,
        entry_search: "",
        import_pending: false,
        import_elapsed_secs: 0,
        import_notice: None,
        glass_mode: false,
    };

    let _element: Element<'_, WorldMessage> = world_dialog_view(&state);
}
