use super::*;

#[test]
fn emotion_prompts_lead_with_the_expression_and_skip_existing_labels() {
    let mut draft = DraftCharacter {
        appearance: "short silver hair".into(),
        age: "45".into(),
        ..DraftCharacter::default()
    };
    let prompt = portrait_prompt(&draft, Some("sad, downcast eyes"));
    assert!(prompt.starts_with("character portrait, sad, downcast eyes, head and shoulders"));
    assert!(prompt.ends_with("45, short silver hair"));
    assert_eq!(missing_emotions(&draft).len(), EMOTIONS.len());
    draft.images.push(CharacterImage {
        id: "a".into(),
        label: " SAD ".into(),
        data: vec![],
    });
    assert!(
        !missing_emotions(&draft)
            .iter()
            .any(|(label, _)| *label == "sad")
    );
    // A full set at the emotion size is expected to fit the broker's byte budget.
    assert!(EMOTIONS.len() <= MAX_CHARACTER_IMAGES);
}

#[test]
fn generated_portraits_are_cropped_and_bounded() {
    let mut png = std::io::Cursor::new(Vec::new());
    ::image::RgbImage::new(1024, 768)
        .write_to(&mut png, ::image::ImageFormat::Png)
        .unwrap();
    let portrait = generated_portrait_png(png.get_ref(), EMOTION_PORTRAIT_SIDE).unwrap();
    let decoded = ::image::load_from_memory(&portrait).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (384, 384));
    assert!(normalize_character_image_png(&portrait).is_ok());
}

#[test]
fn test_draft_character_conversions() {
    let character = Character {
        id: "char-123".into(),
        name: "Lyra".into(),
        description: "A brave warrior".into(),
        personality: "Fierce, loyal".into(),
        scenario: "Ancient ruins".into(),
        system_prompt: "You are Lyra.".into(),
        example_dialogue: "User: Hi\nLyra: Stand ready.".into(),
        appearance: "Armor and spear".into(),
        age: "28".into(),
        gender: "Female".into(),
        race: "Human".into(),
        misc: "Scars from battle".into(),
        tags: vec!["warrior".into(), "human".into()],
        avatar: Some(vec![1, 2, 3]),
        images: vec![CharacterImage {
            id: "img-1".into(),
            label: "combat".into(),
            data: vec![4, 5, 6],
        }],
        default_image_id: Some("img-1".into()),
        is_public: true,
        owned_by_user: true,
        revision: 1,
    };

    let draft = DraftCharacter::from(&character);
    assert_eq!(draft.id.as_deref(), Some("char-123"));
    assert_eq!(draft.name, "Lyra");
    assert_eq!(draft.tags, "warrior, human");
    assert_eq!(draft.images.len(), 1);
    assert!(draft.is_public);
    assert!(draft.owned_by_user);

    // Convert DraftCharacter to CharacterInput
    let input = CharacterInput::from(&draft);
    assert_eq!(input.id.as_deref(), Some("char-123"));
    assert_eq!(input.name, "Lyra");
    assert_eq!(input.tags, vec!["warrior", "human"]);
    // Since images is not empty, avatar should be None according to existing logic
    assert!(input.avatar.is_none());
    assert_eq!(input.images.len(), 1);
    assert!(input.is_public);

    // Convert CharacterInput back to DraftCharacter
    let draft2 = DraftCharacter::from(&input);
    assert_eq!(draft2.name, "Lyra");
    assert_eq!(draft2.tags, "warrior, human");
}

#[test]
fn test_draft_character_update_messages() {
    let mut draft = DraftCharacter::default();

    draft.update(&CharacterMessage::NameChanged("Alice".into()));
    assert_eq!(draft.name, "Alice");

    draft.update(&CharacterMessage::AgeChanged("30".into()));
    assert_eq!(draft.age, "30");

    draft.update(&CharacterMessage::GenderChanged("Female".into()));
    assert_eq!(draft.gender, "Female");

    draft.update(&CharacterMessage::RaceChanged("Elf".into()));
    assert_eq!(draft.race, "Elf");

    // Multi-line fields go through their editor buffers.
    let mut editors = DraftEditors::default();
    draft.description = "Old".into();
    editors.load(&draft);
    assert_eq!(editors.content(LongField::Description).text(), "Old");
    let _ = editors.perform(
        &mut draft,
        LongField::Description,
        text_editor::Action::SelectAll,
    );
    assert_eq!(draft.description, "Old", "selection does not edit");
    for ch in "Mage\nof".chars() {
        let edit = if ch == '\n' {
            text_editor::Edit::Enter
        } else {
            text_editor::Edit::Insert(ch)
        };
        let _ = editors.perform(
            &mut draft,
            LongField::Description,
            text_editor::Action::Edit(edit),
        );
    }
    assert_eq!(draft.description, "Mage\nof");
    let _ = editors.perform(
        &mut draft,
        LongField::Misc,
        text_editor::Action::Edit(text_editor::Edit::Insert('x')),
    );
    assert_eq!(draft.misc, "x");

    draft.update(&CharacterMessage::TagsChanged("magic, spellcaster".into()));
    assert_eq!(draft.tags, "magic, spellcaster");

    draft.update(&CharacterMessage::TogglePublic(true));
    assert!(draft.is_public);

    // Image manipulation
    draft.images.push(CharacterImage {
        id: "i1".into(),
        label: "neutral".into(),
        data: vec![1, 2],
    });
    draft.images.push(CharacterImage {
        id: "i2".into(),
        label: "happy".into(),
        data: vec![3, 4],
    });

    draft.update(&CharacterMessage::SetDefaultImage("i2".into()));
    assert_eq!(draft.default_image_id.as_deref(), Some("i2"));

    draft.update(&CharacterMessage::ImageLabelChanged {
        id: "i1".into(),
        label: "focused".into(),
    });
    assert_eq!(draft.images[0].label, "focused");

    draft.update(&CharacterMessage::DeleteImage("i2".into()));
    assert_eq!(draft.images.len(), 1);
    assert_eq!(draft.default_image_id.as_deref(), Some("i1"));
}

#[test]
fn test_draft_from_card_and_export_roundtrip() {
    let original_draft = DraftCharacter {
        id: None,
        name: "Elysia".into(),
        description: "Adventurous mage".into(),
        personality: "Curious, witty".into(),
        scenario: "Enchanted forest".into(),
        system_prompt: "You are Elysia.".into(),
        example_dialogue: "User: Hi\nElysia: Greetings!".into(),
        appearance: "Green robes, hazel eyes".into(),
        age: "120".into(),
        gender: "Female".into(),
        race: "Elf".into(),
        misc: "Carries a wooden staff".into(),
        tags: "mage, elf, fantasy".into(),
        avatar: None,
        images: vec![],
        default_image_id: None,
        is_public: false,
        owned_by_user: true,
    };

    // Export to SillyTavern JSON v2
    let card = export_card_json(&original_draft);
    assert_eq!(card["spec"], "chara_card_v2");
    assert_eq!(card["spec_version"], "2.0");
    assert_eq!(card["data"]["name"], "Elysia");
    assert_eq!(card["data"]["extensions"]["chatty"]["race"], "Elf");

    // Import back from JSON card
    let imported = draft_from_card(card).expect("should parse valid card");
    assert_eq!(imported.name, original_draft.name);
    assert_eq!(imported.description, original_draft.description);
    assert_eq!(imported.personality, original_draft.personality);
    assert_eq!(imported.scenario, original_draft.scenario);
    assert_eq!(imported.system_prompt, original_draft.system_prompt);
    assert_eq!(imported.example_dialogue, original_draft.example_dialogue);
    assert_eq!(imported.appearance, original_draft.appearance);
    assert_eq!(imported.age, original_draft.age);
    assert_eq!(imported.gender, original_draft.gender);
    assert_eq!(imported.race, original_draft.race);
    assert_eq!(imported.misc, original_draft.misc);
    assert_eq!(imported.tags, original_draft.tags);
}

#[test]
fn zip_export_holds_the_card_and_every_image_and_imports_back() {
    let png = |shade: u8| {
        let mut out = std::io::Cursor::new(Vec::new());
        ::image::RgbImage::from_pixel(128, 128, ::image::Rgb([shade, 0, 0]))
            .write_to(&mut out, ::image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    };
    let draft = DraftCharacter {
        name: "Mara".into(),
        appearance: "silver hair".into(),
        images: vec![
            CharacterImage {
                id: "a".into(),
                label: "neutral".into(),
                data: png(10),
            },
            CharacterImage {
                id: "b".into(),
                label: "very/happy".into(),
                data: png(200),
            },
            CharacterImage {
                id: "c".into(),
                label: "Neutral".into(),
                data: png(90),
            },
        ],
        default_image_id: Some("b".into()),
        ..DraftCharacter::default()
    };

    let bytes = export_card_zip(&draft).unwrap();
    let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    let mut names: Vec<_> = archive.file_names().collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "character.json",
            "images/Neutral-2.png",
            "images/neutral.png",
            "images/very_happy.png"
        ]
    );

    let imported = draft_from_zip(&bytes).unwrap();
    assert_eq!(imported.name, "Mara");
    assert_eq!(imported.appearance, "silver hair");
    let labels: Vec<_> = imported.images.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["neutral", "very/happy", "Neutral"]);
    assert_eq!(imported.images[2].data, draft.images[2].data);
    assert_eq!(imported.images[1].data, draft.images[1].data);
    assert_eq!(
        imported.default_image_id.as_ref(),
        Some(&imported.images[1].id)
    );

    assert!(draft_from_zip(b"not a zip").is_none());
}

#[test]
fn test_draft_from_card_fallback_root() {
    // v1 card where fields are at the root
    let root_card = serde_json::json!({
        "name": "Gerald",
        "description": "A blacksmith",
        "personality": "Gruff but kind",
        "scenario": "The forge",
        "mes_example": "Gerald: What do you need forged?",
        "system_prompt": "You are Gerald the blacksmith.",
    });

    let draft = draft_from_card(root_card).expect("should parse root card");
    assert_eq!(draft.name, "Gerald");
    assert_eq!(draft.description, "A blacksmith");
    assert_eq!(draft.personality, "Gruff but kind");
    assert_eq!(draft.scenario, "The forge");
    assert_eq!(draft.example_dialogue, "Gerald: What do you need forged?");
    assert_eq!(draft.system_prompt, "You are Gerald the blacksmith.");
}

#[test]
fn test_png_card_json_parsing() {
    // Invalid PNG: empty buffer
    assert!(png_card_json(&[]).is_none());

    // Invalid PNG: wrong header
    assert!(png_card_json(b"not a png at all").is_none());

    // Valid PNG header only (no chunks)
    assert!(png_card_json(b"\x89PNG\r\n\x1a\n").is_none());

    // Construct a synthetic PNG with a tEXt chara chunk
    let mut png = Vec::new();
    png.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    // Dummy IHDR chunk (len: 13, type: IHDR, 13 bytes data, 4 bytes crc)
    let ihdr_data = vec![0u8; 13];
    png.extend_from_slice(&(13u32.to_be_bytes()));
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&ihdr_data);
    png.extend_from_slice(&[0, 0, 0, 0]); // CRC

    // Create tEXt chara chunk
    let json_payload = b"{\"name\":\"Hero\",\"personality\":\"Noble\"}";
    let b64_payload = chatty_protocol::util::base64::encode(json_payload);
    let mut text_chunk_data = Vec::new();
    text_chunk_data.extend_from_slice(b"chara\0");
    text_chunk_data.extend_from_slice(b64_payload.as_bytes());

    let chunk_len = text_chunk_data.len() as u32;
    png.extend_from_slice(&chunk_len.to_be_bytes());
    png.extend_from_slice(b"tEXt");
    png.extend_from_slice(&text_chunk_data);
    png.extend_from_slice(&[0, 0, 0, 0]); // CRC

    // Extract with png_card_json
    let extracted = png_card_json(&png).expect("must parse valid tEXt chara chunk");
    assert_eq!(extracted, json_payload);

    // Test non-chara keyword chunk
    let mut png_other_text = Vec::new();
    png_other_text.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let other_data = b"author\0someone";
    png_other_text.extend_from_slice(&(other_data.len() as u32).to_be_bytes());
    png_other_text.extend_from_slice(b"tEXt");
    png_other_text.extend_from_slice(other_data);
    png_other_text.extend_from_slice(&[0, 0, 0, 0]);
    assert!(png_card_json(&png_other_text).is_none());

    // Test truncated chunk
    let mut truncated = png.clone();
    truncated.truncate(png.len() - 5);
    assert!(png_card_json(&truncated).is_none());
}

#[test]
fn test_image_normalization() {
    use ::image::GenericImageView;
    // Valid square PNG 256x256
    let square = ::image::RgbaImage::new(256, 256);
    let mut square_png = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::ImageRgba8(square)
        .write_to(&mut square_png, ::image::ImageFormat::Png)
        .unwrap();

    let norm_square = normalize_character_image_png(square_png.get_ref()).unwrap();
    assert_eq!(norm_square, square_png.into_inner());

    // Rectangular PNG 256x512
    let rect = ::image::RgbaImage::new(256, 512);
    let mut rect_png = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::ImageRgba8(rect)
        .write_to(&mut rect_png, ::image::ImageFormat::Png)
        .unwrap();

    let norm_rect = normalize_character_image_png(rect_png.get_ref()).unwrap();
    let decoded =
        ::image::load_from_memory_with_format(&norm_rect, ::image::ImageFormat::Png).unwrap();
    assert_eq!(decoded.dimensions(), (256, 256));

    // Too small: 64x64
    let small = ::image::RgbaImage::new(64, 64);
    let mut small_png = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::ImageRgba8(small)
        .write_to(&mut small_png, ::image::ImageFormat::Png)
        .unwrap();
    assert!(normalize_character_image_png(small_png.get_ref()).is_err());

    // Too large: 600x600 (side > 512)
    let large = ::image::RgbaImage::new(600, 600);
    let mut large_png = std::io::Cursor::new(Vec::new());
    ::image::DynamicImage::ImageRgba8(large)
        .write_to(&mut large_png, ::image::ImageFormat::Png)
        .unwrap();
    assert!(normalize_character_image_png(large_png.get_ref()).is_err());

    // Corrupt data
    assert!(normalize_character_image_png(b"not png").is_err());
}

#[test]
fn image_cache_reuses_content_and_obeys_lru_entry_limit() {
    let mut cache = CharacterImageCache::default();
    let first = cache.get_or_insert(&[1]).unwrap();
    let second = cache.get_or_insert(&[2]).unwrap();
    for n in 3..=MAX_CACHED_IMAGES as u8 {
        cache.get_or_insert(&[n]).unwrap();
    }
    assert_eq!(cache.get_or_insert(&[1]).unwrap().id(), first.id());
    cache.get_or_insert(&[99]).unwrap();
    assert_eq!(cache.handles.len(), MAX_CACHED_IMAGES);
    assert_eq!(cache.get_or_insert(&[1]).unwrap().id(), first.id());
    assert_ne!(cache.get_or_insert(&[2]).unwrap().id(), second.id());

    // An image edited in place must get a different renderer identity.
    let mut bytes = vec![111, 112];
    let original = cache.get_or_insert(&bytes).unwrap();
    bytes[0] = 113;
    assert_ne!(cache.get_or_insert(&bytes).unwrap().id(), original.id());
}

#[test]
fn image_cache_bounds_bytes_and_checks_hash_collisions() {
    use std::hash::{Hash, Hasher};

    let mut cache = CharacterImageCache::default();
    let first = cache.get_or_insert(&[1, 2, 3]).unwrap();
    let mut hasher = std::hash::DefaultHasher::new();
    [4u8, 5, 6].as_slice().hash(&mut hasher);
    cache.handles[0].0 = hasher.finish();
    assert_ne!(cache.get_or_insert(&[4, 5, 6]).unwrap().id(), first.id());
    cache.clear();
    for n in 1..=3 {
        cache
            .get_or_insert(&vec![n; MAX_CACHED_IMAGE_BYTES / 2])
            .unwrap();
        assert!(cache.retained_bytes <= MAX_CACHED_IMAGE_BYTES);
    }
    assert_eq!(cache.handles.len(), 2);
    let oversized = vec![9; MAX_CACHED_IMAGE_BYTES + 1];
    let first = cache.get_or_insert(&oversized).unwrap();
    let second = cache.get_or_insert(&oversized).unwrap();
    assert_ne!(first.id(), second.id());
    assert_eq!(cache.handles.len(), 2);
    assert_eq!(cache.retained_bytes, MAX_CACHED_IMAGE_BYTES);
    cache.clear();
    assert_eq!(cache.retained_bytes, 0);
    assert!(cache.handles.is_empty());
}

#[test]
#[ignore = "manual image resource measurement"]
fn image_handle_rebuild_resource_probe() {
    let pixels = ::image::RgbaImage::from_fn(256, 256, |x, y| {
        ::image::Rgba([x as u8, y as u8, (x.wrapping_mul(y)) as u8, 255])
    });
    let mut png = std::io::Cursor::new(Vec::new());
    pixels
        .write_to(&mut png, ::image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    for cached in [false, true] {
        clear_image_handles();
        let started = std::time::Instant::now();
        let mut last_id = None;
        let mut decodes = 0;
        for _ in 0..100 {
            let handle = if cached {
                image_handle(&png).unwrap()
            } else {
                iced::widget::image::Handle::from_bytes(png.clone())
            };
            // Model the renderer's ID-keyed decode lookup without opening
            // a window or touching a running application.
            if last_id != Some(handle.id()) {
                std::hint::black_box(::image::load_from_memory(&png).unwrap());
                decodes += 1;
                last_id = Some(handle.id());
            }
        }
        eprintln!(
            "cached={cached}, 100 rebuilds: {:?}, decodes={decodes}, encoded_bytes={}",
            started.elapsed(),
            png.len()
        );
    }
    clear_image_handles();
}

#[test]
fn test_image_handle_helpers_and_cache() {
    assert!(image_handle(&[]).is_none());

    let dummy = vec![1, 2, 3, 4];
    let handle = image_handle(&dummy);
    assert!(handle.is_some());

    let mut cache = CharacterImageCache::default();
    assert!(cache.get_or_insert(&[]).is_none());

    let h1 = cache.get_or_insert(&dummy);
    assert!(h1.is_some());
    let h2 = cache.get_or_insert(&dummy);
    assert!(h2.is_some());

    cache.clear();
    assert_eq!(cache.handles.len(), 0);
}
