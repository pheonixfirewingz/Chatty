use super::{Harness, png};
use crate::characters::{self, CharacterMessage, DraftCharacter};
use crate::network::{Command, Event};
use chatty_protocol::{CharacterImage, Request, Response};

/// The prompt and seed of every image request sent so far.
fn image_requests(app: &mut Harness) -> Vec<(String, Option<u64>)> {
    app.requests()
        .into_iter()
        .filter_map(|request| match request {
            Request::GenerateImage { prompt, seed, .. } => Some((prompt, seed)),
            _ => None,
        })
        .collect()
}

/// Deliver a portrait job result as the network task would.
fn portrait_reply(app: &mut Harness, request_id: u64, result: Result<Vec<u8>, &str>) {
    app.event(Event::ImageRequestStarted {
        request_id,
        scene: false,
    });
    match result {
        Ok(data) => app.respond(request_id, &Response::GeneratedImage { data }),
        Err(message) => app.fail(request_id, message),
    }
}

fn image_generating_app() -> Harness {
    let mut app = Harness::signed_in();
    app.image_generation = true;
    app
}

#[test]
fn emotion_batch_fills_missing_emotions_with_one_seed() {
    let mut app = image_generating_app();
    app.draft.appearance = "silver hair, wool coat".into();
    app.draft.images.push(CharacterImage {
        id: "existing".into(),
        label: "Happy".into(),
        data: png(128),
    });
    app.update(CharacterMessage::GenerateEmotions);
    let [(prompt, seed)] = image_requests(&mut app).try_into().unwrap();
    assert!(prompt.contains("neutral calm expression") && prompt.contains("silver hair"));
    let batch = app.emotion_batch.as_ref().unwrap();
    assert_eq!(
        (batch.done, batch.total),
        (0, characters::EMOTIONS.len() - 1)
    );

    portrait_reply(&mut app, 1, Ok(png(768)));
    let neutral = app.draft.images.last().unwrap();
    assert_eq!(neutral.label, "neutral");
    let size = image::load_from_memory(&neutral.data).unwrap();
    assert_eq!((size.width(), size.height()), (384, 384));
    let [(prompt, next_seed)] = image_requests(&mut app).try_into().unwrap();
    assert!(
        prompt.contains("laughing"),
        "existing Happy image is skipped"
    );
    assert_eq!(next_seed, seed, "the whole set shares one seed");
    assert!(seed.is_some());

    // An error ends the batch and keeps the images already received.
    portrait_reply(&mut app, 2, Err("image generation cancelled"));
    assert!(app.emotion_batch.is_none());
    assert!(!app.portrait_image.pending);
    assert_eq!(app.draft.images.len(), 2);
}

#[test]
fn switching_characters_cancels_an_emotion_batch() {
    let mut app = image_generating_app();
    app.draft.appearance = "red cloak".into();
    app.update(CharacterMessage::GenerateEmotions);
    app.event(Event::ImageRequestStarted {
        request_id: 9,
        scene: false,
    });
    app.update(CharacterMessage::New);
    assert!(app.emotion_batch.is_none());
    assert!(
        app.commands()
            .iter()
            .any(|command| matches!(command, Command::Cancel(9))),
        "the running job is cancelled on the broker"
    );
}

#[test]
fn save_waits_for_running_image_generation() {
    let mut app = image_generating_app();
    app.draft.name = "Fel".into();
    app.draft_character_open = true;
    app.update(CharacterMessage::GeneratePortrait);
    assert_eq!(image_requests(&mut app).len(), 1);
    app.update(CharacterMessage::Save);
    assert!(app.draft_character_open);
    assert!(app.commands().is_empty(), "no save is sent mid-generation");
}

#[test]
fn folder_images_are_added_with_the_folder_named_image_as_default() {
    let mut app = Harness::new();
    let dir = std::env::temp_dir().join(format!("chatty-images-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let paths: Vec<_> = ["happy", "Mara", "tiny"]
        .into_iter()
        .map(|name| {
            let side = if name == "tiny" { 16 } else { 160 };
            let path = dir.join(format!("{name}.png"));
            image::RgbImage::new(side, side).save(&path).unwrap();
            path
        })
        .collect();
    app.import_character_image_paths(paths, Some("mara"));
    std::fs::remove_dir_all(&dir).unwrap();

    let labels: Vec<_> = app.draft.images.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        ["happy", "default"],
        "undersized images are rejected"
    );
    assert_eq!(
        app.draft.default_image_id.as_ref(),
        Some(&app.draft.images[1].id)
    );
    assert!(app.error.is_some(), "the rejected image is reported");
}

#[test]
fn listed_character_images_are_fetched_and_kept() {
    let mut app = Harness::demo();
    app.token = "token".into();
    let mut base = app.characters[0].clone();
    base.id = "c1".into();
    base.images[0].id = "happy".into();
    base.images[0].data.clear();
    base.default_image_id = Some("happy".into());
    let fetches = |app: &mut Harness| {
        app.requests()
            .iter()
            .filter(|request| matches!(request, Request::GetCharacterImage { .. }))
            .count()
    };

    app.set_characters(vec![base.clone()]);
    assert_eq!(fetches(&mut app), 1, "metadata-only listings fetch bytes");
    app.set_characters(vec![base.clone()]);
    assert_eq!(fetches(&mut app), 0, "in-flight fetches are not repeated");

    app.draft = DraftCharacter::from(&app.characters[0]);
    let png = png(128);
    app.store_character_image("c1", "happy", png.clone());
    assert_eq!(app.characters[0].images[0].data, png);
    assert_eq!(app.draft.images[0].data, png, "an open editor shows it too");

    // A post-save refresh keeps the bytes instead of blanking the image.
    app.set_characters(vec![base]);
    assert_eq!(fetches(&mut app), 0);
    assert_eq!(app.characters[0].images[0].data, png);
}

#[test]
fn character_image_fetches_are_paced_with_default_first() {
    let mut app = Harness::demo();
    app.token = "token".into();
    let mut base = app.characters[0].clone();
    base.id = "c-many".into();
    base.images = (0..25)
        .map(|i| CharacterImage {
            id: format!("img-{i}"),
            label: format!("mood {i}"),
            data: Vec::new(),
        })
        .collect();
    base.default_image_id = Some("img-20".into());

    app.set_characters(vec![base]);
    let batch1: Vec<_> = app
        .requests()
        .into_iter()
        .filter_map(|request| match request {
            Request::GetCharacterImage {
                character_id,
                image_id,
                ..
            } => Some((character_id, image_id)),
            _ => None,
        })
        .collect();
    assert_eq!(batch1.len(), 2, "at most 2 fetches start immediately");
    assert_eq!(batch1[0].1, "img-20", "default portrait is fetched first");

    // A normal response frees one slot and advances the queue.
    let sample = png(128);
    app.event(Event::CharacterImageRequestStarted {
        request_id: 100,
        character_id: batch1[0].0.clone(),
        image_id: batch1[0].1.clone(),
    });
    app.respond(
        100,
        &Response::CharacterImage {
            character_id: batch1[0].0.clone(),
            image_id: batch1[0].1.clone(),
            data: sample.clone(),
        },
    );
    let batch2: Vec<_> = app
        .requests()
        .into_iter()
        .filter_map(|request| match request {
            Request::GetCharacterImage { image_id, .. } => Some(image_id),
            _ => None,
        })
        .collect();
    assert_eq!(batch2.len(), 1, "exactly 1 replacement fetch starts");

    // A per-image error frees its slot without showing a modal error.
    app.event(Event::CharacterImageRequestStarted {
        request_id: 101,
        character_id: batch1[1].0.clone(),
        image_id: batch1[1].1.clone(),
    });
    app.fail(101, "transient error");
    assert!(app.error.is_none());

    let mut pending = vec![batch2[0].clone()];
    let next: Vec<_> = app
        .requests()
        .into_iter()
        .filter_map(|request| match request {
            Request::GetCharacterImage { image_id, .. } => Some(image_id),
            _ => None,
        })
        .collect();
    assert_eq!(next.len(), 1);
    pending.extend(next);

    let mut req_id = 102;
    let mut completed = 1;
    while let Some(image_id) = pending.pop() {
        app.event(Event::CharacterImageRequestStarted {
            request_id: req_id,
            character_id: "c-many".into(),
            image_id: image_id.clone(),
        });
        app.respond(
            req_id,
            &Response::CharacterImage {
                character_id: "c-many".into(),
                image_id,
                data: sample.clone(),
            },
        );
        req_id += 1;
        completed += 1;
        for request in app.requests() {
            if let Request::GetCharacterImage { image_id, .. } = request {
                pending.push(image_id);
            }
        }
    }
    assert_eq!(completed, 24);
    assert_eq!(app.characters[0].images[20].data, sample);
}

#[test]
fn saving_new_character_reuses_pending_saved_images_without_refetching() {
    let mut app = Harness::signed_in();
    app.draft_character_open = true;
    app.draft = DraftCharacter {
        id: None,
        name: "Mara".into(),
        images: (0..22)
            .map(|i| CharacterImage {
                id: format!("img-{i}"),
                label: format!("pose {i}"),
                data: png(128),
            })
            .collect(),
        default_image_id: Some("img-0".into()),
        ..DraftCharacter::default()
    };

    app.update(CharacterMessage::Save);
    assert!(!app.draft_character_open);
    assert_eq!(app.pending_saved_images.len(), 22);
    let _ = app.requests();

    // Broker assigns a new character ID and responds with metadata-only listings.
    let listed = chatty_protocol::Character {
        id: "new-char-id".into(),
        name: "Mara".into(),
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
        images: (0..22)
            .map(|i| CharacterImage {
                id: format!("img-{i}"),
                label: format!("pose {i}"),
                data: Vec::new(),
            })
            .collect(),
        default_image_id: Some("img-0".into()),
        is_public: false,
        owned_by_user: true,
        revision: 1,
    };
    app.set_characters(vec![listed]);
    assert!(
        !app.requests()
            .iter()
            .any(|request| matches!(request, Request::GetCharacterImage { .. })),
        "newly saved images must be reused without refetching"
    );
    assert!(app.pending_saved_images.is_empty());
    assert!(
        app.characters[0]
            .images
            .iter()
            .all(|image| !image.data.is_empty())
    );
}

#[test]
fn stale_image_batch_and_reentrant_file_dialogs_are_ignored() {
    let mut app = Harness::signed_in();
    app.draft_character_open = true;
    let old_epoch = app.draft_epoch;

    app.file_dialog_open = true;
    assert_eq!(app.update_units(CharacterMessage::AddImages), 0);
    assert_eq!(app.update_units(CharacterMessage::Import), 0);
    assert_eq!(
        app.update_units(crate::worlds::WorldMessage::ImportLorebook),
        0
    );
    assert_eq!(
        app.update_units(crate::admin::AdminMessage::LoadImageWorkflow),
        0
    );

    // Switching drafts bumps draft_epoch so late image batches are dropped.
    app.update(CharacterMessage::New);
    app.update(CharacterMessage::ImagesLoaded {
        epoch: old_epoch,
        batch: Some(Ok(characters::ImportedImageBatch {
            images: vec![CharacterImage {
                id: "stale".into(),
                label: "stale".into(),
                data: png(128),
            }],
            default_image_id: Some("stale".into()),
            errors: Vec::new(),
            truncated: false,
        })),
    });
    assert!(!app.file_dialog_open);
    assert!(app.draft.images.is_empty());
}
