//! Data paths, boot log, delta fan-out, cancellation, and TLS helpers.
use super::*;

#[test]
fn database_uses_absolute_xdg_data_home() {
    assert_eq!(
        default_user_data_dir(
            Some(PathBuf::from("/tmp/xdg-data")),
            Some(PathBuf::from("/home/tester")),
        ),
        Some(PathBuf::from("/tmp/xdg-data/chatty")),
    );
}

#[test]
fn database_uses_linux_home_fallback() {
    assert_eq!(
        default_user_data_dir(None, Some(PathBuf::from("/home/tester"))),
        Some(PathBuf::from("/home/tester/.local/share/chatty")),
    );
}

#[test]
fn database_never_falls_back_to_current_directory() {
    assert_eq!(default_user_data_dir(None, None), None);
    assert_eq!(
        default_user_data_dir(
            Some(PathBuf::from("relative/data")),
            Some(PathBuf::from("relative/home")),
        ),
        None,
    );
}

#[test]
fn boot_log_is_truncated_when_opened_for_a_new_broker_run() {
    let path = std::env::temp_dir().join(format!("chatty-boot-log-{}.log", new_uuid()));
    fs::write(&path, "previous boot\n").unwrap();
    drop(open_boot_log(&path).unwrap());
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
    let permissions = fs::metadata(&path).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(permissions.mode() & 0o777, 0o600);
    }
    fs::remove_file(path).unwrap();
}

#[test]
fn live_delta_visibility_is_owner_and_origin_scoped() {
    let origin = new_uuid();
    let peer = new_uuid();
    let event = PublishedDelta {
        owner_id: "owner-a".into(),
        origin: origin.clone(),
        encoded: Bytes::new(),
    };
    assert!(delta_visible(Some("owner-a"), &peer, &event));
    assert!(!delta_visible(Some("owner-b"), &peer, &event));
    assert!(!delta_visible(None, &peer, &event));
    assert!(!delta_visible(Some("owner-a"), &origin, &event));
}

#[tokio::test]
async fn cancellation_registry_stress_is_connection_scoped() {
    let registry: CancellationRegistry = Arc::new(Mutex::new(HashMap::new()));
    let target = new_uuid();
    let survivor = new_uuid();
    let mut receivers = Vec::new();
    {
        let mut map = registry.lock().await;
        for request_id in 0..1_000 {
            let (sender, receiver) = watch::channel(false);
            map.insert((target.clone(), request_id), sender);
            receivers.push(receiver);
        }
        let (sender, _) = watch::channel(false);
        map.insert((survivor.clone(), 1), sender);
    }
    cancel_connection(&registry, target).await;
    assert_eq!(registry.lock().await.len(), 1);
    assert!(registry.lock().await.contains_key(&(survivor, 1)));
    assert!(receivers.iter().all(|receiver| *receiver.borrow()));
}

#[test]
fn certificate_fingerprint_matches_the_gui_format() {
    assert_eq!(
        certificate_fingerprint(b"abc"),
        "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD"
    );
}

#[test]
fn cleartext_inference_warning_ignores_loopback_and_https() {
    assert!(sends_cleartext_off_host("http://192.168.0.97:11434/v1"));
    assert!(sends_cleartext_off_host("http://inference.example/v1"));
    assert!(!sends_cleartext_off_host("https://192.168.0.97:11434/v1"));
    assert!(!sends_cleartext_off_host("http://127.0.0.1:11434/v1"));
    assert!(!sends_cleartext_off_host("http://localhost:11434/v1"));
    assert!(!sends_cleartext_off_host("http://[::1]:11434/v1"));
}
