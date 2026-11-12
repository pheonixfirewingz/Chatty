use super::*;
use crate::inference::tests::auxiliary_completion_tests::test_app;

#[tokio::test]
async fn deleted_scene_image_leaves_the_timeline() {
    let app = test_app("http://127.0.0.1:9").await;
    let (id, _, _) = save_scene_image(&app, "owner", "chat", "a lake", b"png")
        .await
        .unwrap();
    assert_eq!(
        load_message_image(&app, "owner", &id).await.unwrap(),
        b"png"
    );
    let mut transaction = app.db.begin().await.unwrap();
    delete_owned_entity(&mut transaction, "owner", EntityKind::Message, &id)
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let view = load_owned_conversation(&app.db, "chat", "owner")
        .await
        .unwrap()
        .unwrap();
    assert!(view.messages.is_empty());
    assert!(load_message_image(&app, "owner", &id).await.is_err());
}
