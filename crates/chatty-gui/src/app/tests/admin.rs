use super::{Harness, png};
use crate::admin::AdminMessage;
use crate::app::Screen;
use crate::network::{Command, Event};
use chatty_protocol::admin::ModelDownloadProgress;
use chatty_protocol::{Request, Response};
use std::time::{Duration, Instant};

#[test]
fn admin_monitor_refreshes_only_while_open() {
    let mut app = Harness::signed_in();
    app.screen = Screen::Admin;
    app.admin.admin_tab = 0;
    app.last_monitor_refresh = Some(Instant::now() - Duration::from_secs(3));
    app.refresh_admin_monitor_if_due();
    assert!(matches!(
        app.requests().as_slice(),
        [Request::AdminGetBrokerMonitor { .. }]
    ));

    app.screen = Screen::Chat;
    app.last_monitor_refresh = Some(Instant::now() - Duration::from_secs(3));
    app.refresh_admin_monitor_if_due();
    assert!(app.commands().is_empty());
}

#[test]
fn model_request_errors_recover_only_the_matching_operation() {
    let mut app = Harness::new();
    app.admin.models.busy = true;
    app.admin.models.request_id = Some(42);
    app.fail(41, "Download failed: disk full");
    assert!(app.admin.models.busy);
    app.fail(42, "Download failed: disk full");
    assert!(!app.admin.models.busy);
    assert_eq!(
        app.admin.models.notice.as_deref(),
        Some("Download failed: disk full")
    );
    app.admin.models.busy = true;
    app.event(Event::Status("Offline: connection lost".into()));
    assert!(!app.admin.models.busy);
}

#[test]
fn download_progress_cancellation_and_errors_are_request_scoped() {
    let mut app = Harness::new();
    for model in ["a", "b"] {
        app.admin
            .update(AdminMessage::DownloadModel(model.into()), "session");
    }
    for (request_id, model) in [(41, "a"), (42, "b")] {
        app.event(Event::DownloadStarted {
            request_id,
            model: model.into(),
        });
    }
    let progress = ModelDownloadProgress {
        status: "pulling layer".into(),
        total: Some(100),
        completed: Some(25),
        digest: None,
    };
    app.respond(41, &Response::ModelDownloadProgress(progress));
    assert_eq!(app.admin.models.downloads["a"].progress.completed, Some(25));
    assert_eq!(app.admin.models.downloads["b"].progress.completed, None);
    assert!(!app.admin.models.busy);

    app.update(AdminMessage::CancelDownload("a".into()));
    assert!(matches!(app.commands().as_slice(), [Command::Cancel(41)]));
    app.respond(41, &Response::ModelDownloadFinished { cancelled: true });
    assert!(app.admin.models.downloads["a"].finished);
    assert!(!app.admin.models.downloads["b"].finished);
    app.fail(42, "disk full");
    assert_eq!(app.admin.models.downloads["b"].progress.status, "disk full");
    assert!(app.admin.models.downloads["b"].finished);

    // Cancelling before the network assigns an ID must still cancel upstream.
    app.admin
        .update(AdminMessage::DownloadModel("a".into()), "session");
    app.update(AdminMessage::CancelDownload("a".into()));
    assert!(app.commands().is_empty());
    app.event(Event::DownloadStarted {
        request_id: 43,
        model: "a".into(),
    });
    assert!(matches!(app.commands().as_slice(), [Command::Cancel(43)]));
    // A late terminal event from the previous attempt cannot finish its retry.
    app.respond(41, &Response::ModelDownloadFinished { cancelled: false });
    assert!(!app.admin.models.downloads["a"].finished);
    app.event(Event::Status("Offline: connection lost".into()));
    assert!(app.admin.models.downloads["a"].finished);
}

#[test]
fn admin_test_render_receives_its_image() {
    let mut app = Harness::signed_in();
    app.update(AdminMessage::TestImageGeneration);
    assert!(matches!(
        app.requests().as_slice(),
        [Request::GenerateImage { .. }]
    ));
    app.event(Event::ImageRequestStarted {
        request_id: 4,
        scene: false,
    });
    app.respond(4, &Response::GeneratedImage { data: png(256) });
    assert!(!app.admin.image_test.pending);
    assert!(app.admin.image_test.result.is_some());
    assert!(app.admin.image_test.elapsed.is_some());
    assert!(
        app.draft.images.is_empty(),
        "test renders never reach a character"
    );
}

#[test]
fn closing_the_admin_portal_returns_to_chat() {
    let mut app = Harness::signed_in();
    app.screen = Screen::Admin;
    app.update(AdminMessage::CloseDialog);
    assert_eq!(app.screen, Screen::Chat);
}
