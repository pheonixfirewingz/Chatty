use super::address::{broker_host, connection_target};
use super::session::{
    SavedSession, SpeechPreferences, default_session_path, last_server_path, load_last_server,
    load_session, load_speech_preferences, save_last_server, save_preferences, save_session,
};
use super::tls::{
    CertificateVerifier, PendingCertificate, certificate_fingerprint, default_server_ca_path,
};
use super::{is_expected_post_logout_unauthorized, take_accepted_generation};
use chatty_protocol::{ErrorCode, Request, Response, WireError};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

#[test]
fn closed_idle_connection_is_normal_but_other_failures_remain_visible() {
    let closed = chatty_protocol::ProtocolError::Io(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "peer closed connection without sending TLS close_notify",
    ));
    assert_eq!(
        super::connection_closed_status(&closed),
        "Idle · reconnects on demand"
    );

    let failure = chatty_protocol::ProtocolError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "invalid TLS record",
    ));
    assert!(
        super::connection_closed_status(&failure)
            .contains("Offline: I/O error: invalid TLS record")
    );
}

#[tokio::test]
async fn idle_connection_waits_for_and_preserves_the_next_command() {
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut pending = std::collections::VecDeque::new();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            super::park_for_command(&mut pending, &mut receiver),
        )
        .await
        .is_err()
    );
    sender
        .send(super::Command::Request(Box::new(
            Request::GetServerCapabilities,
        )))
        .unwrap();
    assert!(
        super::park_for_command(&mut pending, &mut receiver)
            .await
            .is_some()
    );
    assert!(matches!(
        super::next_command(&mut pending, &mut receiver).await,
        Some(super::Command::Request(request)) if matches!(*request, Request::GetServerCapabilities)
    ));
    drop(sender);
    assert!(
        super::park_for_command(&mut pending, &mut receiver)
            .await
            .is_none()
    );
}

#[test]
fn generation_is_released_only_after_its_user_message_is_accepted() {
    let mut pending = HashMap::new();
    pending.insert(
        21,
        Box::new(Request::Generate {
            session_token: "session".into(),
            conversation_id: "conversation".into(),
            speaker_id: Some("character".into()),
            parent_id: None,
        }),
    );
    let other_accepted = Response::Accepted {
        entity_id: Some("other".into()),
        revision: 1,
    };
    assert!(take_accepted_generation(Some(&other_accepted), 20, false, &mut pending).is_none());

    let accepted = Response::Accepted {
        entity_id: Some("message".into()),
        revision: 2,
    };
    assert!(matches!(
        take_accepted_generation(
            Some(&accepted),
            21,
            false,
            &mut pending
        ),
        Some(request) if matches!(*request, Request::Generate { .. })
    ));
    assert!(pending.is_empty());
}

#[test]
fn certificate_fingerprint_is_colon_separated_sha256() {
    assert_eq!(
        certificate_fingerprint(b""),
        "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
    );
}

#[test]
fn pinned_verifier_rejects_a_changed_certificate() {
    let verifier = CertificateVerifier {
        expected: Some(vec![1, 2, 3]),
        provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
    };
    let expected = rustls::pki_types::CertificateDer::from(vec![1, 2, 3]);
    let changed = rustls::pki_types::CertificateDer::from(vec![1, 2, 4]);
    let name = rustls::pki_types::ServerName::try_from("private.example".to_owned()).unwrap();
    let now = rustls::pki_types::UnixTime::since_unix_epoch(Duration::from_secs(0));

    assert!(
        rustls::client::danger::ServerCertVerifier::verify_server_cert(
            &verifier,
            &expected,
            &[],
            &name,
            &[],
            now,
        )
        .is_ok()
    );
    assert!(
        rustls::client::danger::ServerCertVerifier::verify_server_cert(
            &verifier,
            &changed,
            &[],
            &name,
            &[],
            now,
        )
        .is_err()
    );
}

#[test]
fn enrollment_uses_the_last_certificate_as_a_private_ca() {
    let chain = [
        rustls::pki_types::CertificateDer::from(vec![1, 2, 3]),
        rustls::pki_types::CertificateDer::from(vec![4, 5, 6]),
    ];
    let pending = PendingCertificate::from_chain(
        super::ConnectionTarget {
            broker: "private.example:7443".into(),
            server_name: "private.example".into(),
        },
        &chain,
    )
    .unwrap();

    assert!(pending.is_ca());
    assert_eq!(pending.certificate, vec![4, 5, 6]);
    assert_eq!(pending.fingerprint, certificate_fingerprint(&[4, 5, 6]));
}

#[test]
fn session_uses_absolute_xdg_state_home() {
    assert_eq!(
        default_session_path(
            Some(PathBuf::from("/tmp/xdg-state")),
            Some(PathBuf::from("/home/tester")),
        ),
        Some(PathBuf::from("/tmp/xdg-state/chatty/session")),
    );
}

#[test]
fn session_uses_linux_home_fallback() {
    assert_eq!(
        default_session_path(None, Some(PathBuf::from("/home/tester"))),
        Some(PathBuf::from("/home/tester/.local/state/chatty/session")),
    );
}

#[test]
fn relative_xdg_state_home_is_ignored() {
    assert_eq!(
        default_session_path(
            Some(PathBuf::from("relative/state")),
            Some(PathBuf::from("/home/tester")),
        ),
        Some(PathBuf::from("/home/tester/.local/state/chatty/session")),
    );
}

#[test]
fn session_never_falls_back_to_current_directory() {
    assert_eq!(default_session_path(None, None), None);
    assert_eq!(
        default_session_path(
            Some(PathBuf::from("relative/state")),
            Some(PathBuf::from("relative/home")),
        ),
        None,
    );
}

#[test]
fn unauthorized_from_pre_logout_request_is_expected() {
    let error = WireError {
        code: ErrorCode::Unauthorized,
        message: "unauthorized".into(),
        retryable: false,
    };

    assert!(is_expected_post_logout_unauthorized(
        12,
        Some(&error),
        Some(12)
    ));
    assert!(!is_expected_post_logout_unauthorized(
        12,
        Some(&error),
        Some(11)
    ));
    assert!(!is_expected_post_logout_unauthorized(
        12,
        Some(&error),
        None
    ));
    assert!(!is_expected_post_logout_unauthorized(12, None, Some(12)));
}

#[test]
fn speech_preferences_round_trip() {
    let path =
        std::env::temp_dir().join(format!("chatty-speech-preferences-{}", std::process::id()));
    assert_eq!(load_speech_preferences(&path), SpeechPreferences::default());
    assert!(SpeechPreferences::default().dialogue_only);
    for speech in [
        SpeechPreferences {
            auto_speak: true,
            dialogue_only: false,
        },
        SpeechPreferences {
            auto_speak: false,
            dialogue_only: true,
        },
    ] {
        save_preferences(&path, speech);
        assert_eq!(load_speech_preferences(&path), speech);
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn legacy_appearance_preferences_preserve_speech_setting() {
    let path =
        std::env::temp_dir().join(format!("chatty-legacy-appearance-{}", std::process::id()));
    std::fs::write(
        &path,
        "theme=light\nsurface=solid\ntransparency=80\ntts_auto_speak=true\n",
    )
    .unwrap();
    let speech = load_speech_preferences(&path);
    assert!(speech.auto_speak && speech.dialogue_only);
    save_preferences(&path, speech);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "tts_auto_speak=true\ntts_dialogue_only=true\n"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn saved_session_is_scoped_to_its_broker() {
    let path = std::env::temp_dir().join(format!("chatty-saved-session-{}", std::process::id()));
    let session = SavedSession {
        broker: "192.168.0.98:7443".into(),
        token: "secret-session-token".into(),
    };

    save_session(&path, &session);

    assert_eq!(load_session(&path, None), Some(session));
    let _ = std::fs::remove_file(path);
}

#[test]
fn legacy_session_requires_an_explicit_startup_broker() {
    let path = std::env::temp_dir().join(format!("chatty-legacy-session-{}", std::process::id()));
    std::fs::write(&path, "old-unscoped-token").unwrap();

    assert_eq!(load_session(&path, None), None);
    assert_eq!(
        load_session(&path, Some("192.168.0.98:7443")),
        Some(SavedSession {
            broker: "192.168.0.98:7443".into(),
            token: "old-unscoped-token".into(),
        })
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn server_ca_uses_absolute_xdg_config_home() {
    assert_eq!(
        default_server_ca_path(
            "192.168.0.98",
            Some(PathBuf::from("/tmp/xdg-config")),
            Some(PathBuf::from("/home/test")),
        ),
        Some(PathBuf::from(
            "/tmp/xdg-config/chatty/server-cas/192.168.0.98.ca.pem"
        )),
    );
}

#[test]
fn server_ca_falls_back_to_home_config() {
    assert_eq!(
        default_server_ca_path(
            "broker.example.test",
            Some(PathBuf::from("relative-config")),
            Some(PathBuf::from("/home/test")),
        ),
        Some(PathBuf::from(
            "/home/test/.config/chatty/server-cas/broker.example.test.ca.pem"
        )),
    );
}

#[test]
fn last_connected_server_round_trips() {
    let session =
        std::env::temp_dir().join(format!("chatty-last-server-session-{}", std::process::id()));
    let path = last_server_path(&session);

    save_last_server(&path, "  chatty.example.test  ");

    assert_eq!(
        load_last_server(&path).as_deref(),
        Some("chatty.example.test")
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn broker_host_accepts_ips_and_domains() {
    assert_eq!(broker_host("192.168.0.98"), Some("192.168.0.98".into()));
    assert_eq!(
        broker_host("chatty.example.com"),
        Some("chatty.example.com".into())
    );
    assert_eq!(
        broker_host("192.168.0.98:7443"),
        Some("192.168.0.98".into())
    );
    assert_eq!(
        broker_host("chatty.example.com:7443"),
        Some("chatty.example.com".into())
    );
    assert_eq!(broker_host("https://chatty.example.com"), None);
    assert_eq!(broker_host("chatty.example.com:8080"), None);
    assert_eq!(broker_host("-bad.domain"), None);
}

#[test]
fn connection_target_uses_tls_host_and_default_port() {
    let ipv4 = connection_target("192.168.0.98").unwrap();
    assert_eq!(ipv4.broker, "192.168.0.98:7443");
    assert_eq!(ipv4.server_name, "192.168.0.98");

    let domain = connection_target("chatty.example.com").unwrap();
    assert_eq!(domain.broker, "chatty.example.com:7443");
    assert_eq!(domain.server_name, "chatty.example.com");
}
