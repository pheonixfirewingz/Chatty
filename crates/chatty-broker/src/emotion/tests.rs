use super::*;

fn portraits() -> Vec<(String, String)> {
    ["neutral", "happy", "angry", "Joy"]
        .iter()
        .map(|label| (format!("portrait-{label}"), (*label).to_owned()))
        .collect()
}

#[test]
fn parses_the_emotion_list_once_it_closes() {
    assert_eq!(
        parse_emotions("```json\n{\n  \"emotions\": [\"Anger\","),
        None
    );
    assert_eq!(
        parse_emotions("```json\n{\n  \"emotions\": [ \"Anger\", \" Irritation \" ],\n  \"expl"),
        Some(vec!["Anger".to_owned(), "Irritation".to_owned()])
    );
    assert_eq!(parse_emotions("{\"emotions\": [3]}"), None);
}

#[test]
fn maps_emotions_to_portraits_in_model_order() {
    let portraits = portraits();
    let pick = |emotions: &[&str]| {
        let emotions = emotions.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>();
        portrait_for_emotions(&portraits, &emotions).map(str::to_owned)
    };
    // A custom label matching the classifier's emotion wins over the map.
    assert_eq!(pick(&["joy"]).as_deref(), Some("portrait-Joy"));
    assert_eq!(pick(&["Frustration"]).as_deref(), Some("portrait-angry"));
    // Emotions without a portrait fall through to the next one.
    assert_eq!(
        pick(&["Fear", "Contentment"]).as_deref(),
        Some("portrait-happy")
    );
    assert_eq!(pick(&["Fear"]), None);
    assert_eq!(pick(&[]), None);
}

#[tokio::test]
async fn portrait_updates_only_the_unchanged_visible_reply() {
    let app =
        crate::inference::tests::auxiliary_completion_tests::test_app("http://127.0.0.1:9").await;
    for (id, parent, selected, image) in [
        ("reply", None::<&str>, None::<&str>, "portrait-neutral"),
        ("edited", None, None, "portrait-angry"),
        ("parent", None, Some("old-variant"), "portrait-neutral"),
    ] {
        sqlx::query("INSERT INTO messages(id,conversation_id,author_type,author_id,content,parent_id,selected_variant_id,character_image_id,revision) VALUES(?,'chat','character','c1','hi',?,?,?,1)")
            .bind(id).bind(parent).bind(selected).bind(image)
            .execute(&app.db).await.unwrap();
    }
    let job = |message_id: &str, generation_id: &str| PortraitJob {
        owner_id: "owner".into(),
        message_id: message_id.into(),
        generation_id: generation_id.into(),
        reply: "hi".into(),
        portraits: portraits(),
        current_image_id: Some("portrait-neutral".into()),
    };
    let image = |id: &str| {
        let db = app.db.clone();
        let id = id.to_owned();
        async move {
            sqlx::query_scalar::<_, String>("SELECT character_image_id FROM messages WHERE id=?")
                .bind(id)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };

    let delta = persist_portrait(&app, &job("reply", "reply"), "portrait-happy")
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(delta.operation, DeltaOperation::Update));
    let Ok(DeltaPayload::Message {
        character_image_id, ..
    }) = decode::<DeltaPayload>(&delta.changed_fields)
    else {
        panic!("message delta expected")
    };
    assert_eq!(character_image_id.as_deref(), Some("portrait-happy"));
    assert_eq!(image("reply").await, "portrait-happy");

    // A portrait changed since the reply was saved is left alone.
    assert!(
        persist_portrait(&app, &job("edited", "edited"), "portrait-happy")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(image("edited").await, "portrait-angry");
    // A variant no longer selected does not own the message portrait.
    assert!(
        persist_portrait(&app, &job("parent", "stale-variant"), "portrait-happy")
            .await
            .unwrap()
            .is_none()
    );
    // Other owners and deleted messages are ignored.
    let mut foreign = job("reply", "reply");
    foreign.owner_id = "intruder".into();
    foreign.current_image_id = Some("portrait-happy".into());
    assert!(
        persist_portrait(&app, &foreign, "portrait-angry")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        persist_portrait(&app, &job("gone", "gone"), "portrait-happy")
            .await
            .unwrap()
            .is_none()
    );
}

/// Needs `CHATTY_EMOTION_MODEL` pointing at EmotionTextClassifierLLM.gguf.
#[test]
#[ignore]
fn live_classifier_picks_distinct_portraits() {
    let path = std::env::var("CHATTY_EMOTION_MODEL").unwrap();
    let runtime = runtime().unwrap();
    let portraits = portraits();
    for (reply, expected) in [
        (
            "*beams and claps* This is the best day of my life!",
            "portrait-Joy",
        ),
        (
            "*slams the table* How dare you lie to me again!",
            "portrait-angry",
        ),
    ] {
        let emotions = classify(runtime, &path, reply).unwrap();
        assert_eq!(
            portrait_for_emotions(&portraits, &emotions),
            Some(expected),
            "{emotions:?}"
        );
    }
}

#[test]
fn every_mapped_label_is_a_generated_portrait_label() {
    const GENERATED: [&str; 12] = [
        "neutral",
        "happy",
        "laughing",
        "sad",
        "angry",
        "surprised",
        "scared",
        "embarrassed",
        "flirty",
        "thoughtful",
        "smug",
        "disgusted",
    ];
    for (emotion, label) in PORTRAIT_LABELS {
        assert!(
            GENERATED.contains(&label),
            "{emotion} maps to unknown {label}"
        );
        assert!(SYSTEM_PROMPT.to_lowercase().contains(emotion), "{emotion}");
    }
}
