//! Character storage and encrypted portraits.
use super::*;

#[test]
fn character_images_are_authenticated_and_encrypted_at_rest() {
    let key = [9u8; 32];
    let images = vec![CharacterImage {
        id: "joy".into(),
        label: "joy".into(),
        data: b"licensed portrait bytes".to_vec(),
    }];

    let encrypted = encrypt_character_images(&key, "owner", "character", &images).unwrap();
    assert!(encrypted.starts_with(b"CHIMG1"));
    assert!(
        !encrypted
            .windows(images[0].data.len())
            .any(|window| window == images[0].data)
    );
    assert_eq!(
        decrypt_character_images(&key, "owner", "character", &encrypted).unwrap(),
        images
    );
    assert!(decrypt_character_images(&key, "other-owner", "character", &encrypted).is_err());
}

#[tokio::test]
async fn saving_listing_and_concurrently_fetching_many_character_images_succeeds() {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO broker_settings(singleton,adapter_url) VALUES(1,'http://127.0.0.1:1/v1')",
    )
    .execute(&db)
    .await
    .unwrap();
    let app = App::new(db, test_adapter_host(), PathBuf::new(), [7; 32]);

    let (_, registered) = call(
        &app,
        Request::Register {
            username: "artist".into(),
            password: "password-123".into(),
        },
    )
    .await
    .unwrap();
    let Response::Authenticated { session_token, .. } = decode(&registered).unwrap() else {
        panic!("expected authentication");
    };

    let images: Vec<CharacterImage> = (0..26)
        .map(|i| {
            let mut png = std::io::Cursor::new(Vec::new());
            image::RgbImage::from_pixel(128, 128, image::Rgb([(i + 1) as u8, 0, 0]))
                .write_to(&mut png, image::ImageFormat::Png)
                .unwrap();
            CharacterImage {
                id: format!("img-{i}"),
                label: format!("expr-{i}"),
                data: png.into_inner(),
            }
        })
        .collect();
    let (_, saved) = call(
        &app,
        Request::UpsertCharacter {
            session_token: session_token.clone(),
            character: Box::new(CharacterInput {
                id: None,
                name: "ManyImages".into(),
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
                images: images.clone(),
                default_image_id: Some("img-0".into()),
                is_public: false,
                owned_by_user: true,
            }),
            world_ids: Vec::new(),
        },
    )
    .await
    .unwrap();
    let Response::Accepted {
        entity_id: Some(character_id),
        ..
    } = decode(&saved).unwrap()
    else {
        panic!("expected accepted character save");
    };

    let (_, listed) = call(
        &app,
        Request::ListCharacters {
            session_token: session_token.clone(),
        },
    )
    .await
    .unwrap();
    let Response::Characters(chars) = decode(&listed).unwrap() else {
        panic!("expected character list");
    };
    assert_eq!(chars.len(), 1);
    assert_eq!(chars[0].images.len(), 26);
    assert!(chars[0].images.iter().all(|img| img.data.is_empty()));

    // Concurrent burst fetch for all 26 images succeeds under image_decrypt_gate.
    let fetches: Vec<_> = (0..26)
        .map(|i| {
            let app = app.clone();
            let session_token = session_token.clone();
            let character_id = character_id.clone();
            async move {
                let (_, payload) = call(
                    &app,
                    Request::GetCharacterImage {
                        session_token,
                        character_id,
                        image_id: format!("img-{i}"),
                    },
                )
                .await
                .unwrap();
                let Response::CharacterImage { image_id, data, .. } = decode(&payload).unwrap()
                else {
                    panic!("expected CharacterImage response");
                };
                (i, image_id, data)
            }
        })
        .collect();
    for (i, image_id, data) in futures_util::future::join_all(fetches).await {
        assert_eq!(image_id, format!("img-{i}"));
        assert_eq!(data, images[i].data);
    }

    // Re-saving metadata with empty image bytes preserves stored images.
    let metadata_only: Vec<CharacterImage> = images
        .iter()
        .map(|img| CharacterImage {
            id: img.id.clone(),
            label: format!("updated-{}", img.label),
            data: Vec::new(),
        })
        .collect();
    call(
        &app,
        Request::UpsertCharacter {
            session_token: session_token.clone(),
            character: Box::new(CharacterInput {
                id: Some(character_id.clone()),
                name: "ManyImages".into(),
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
                images: metadata_only,
                default_image_id: Some("img-25".into()),
                is_public: false,
                owned_by_user: true,
            }),
            world_ids: Vec::new(),
        },
    )
    .await
    .unwrap();
    let (_, payload) = call(
        &app,
        Request::GetCharacterImage {
            session_token,
            character_id,
            image_id: "img-25".into(),
        },
    )
    .await
    .unwrap();
    let Response::CharacterImage { data, .. } = decode(&payload).unwrap() else {
        panic!("expected CharacterImage response");
    };
    assert_eq!(data, images[25].data);
}
