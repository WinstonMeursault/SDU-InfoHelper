//! Commands validate the feature they use; check-config retains full validation.
use sdu_infohelper::{settings::Settings, with_dorm_auth};
use std::{fs, path::Path, process::Command, time::Duration};
mod common;

fn cli(config: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"))
        .args(args)
        .arg("--config")
        .arg(config)
        .output()
        .unwrap()
}

#[test]
fn authentication_status_ignores_unrelated_semantic_errors_but_full_validation_rejects_them() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    for unrelated in [
        "daemon:\n  threshold_kwh: '-1'\n",
        "notifications:\n  channels:\n    - type: webhook\n      id: test\n      url: invalid-url\n",
    ] {
        fs::write(&config, unrelated).unwrap();
        assert!(Settings::read(&config).is_ok());
        assert!(Settings::load(&config).is_err());
        let status = cli(&config, &["auth", "status"]);
        assert!(status.status.success(), "{status:?}");
        let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(json["cached"], false);
        assert!(!cli(&config, &["check-config"]).status.success());
    }
}

#[test]
fn authenticated_dorm_operations_only_require_query_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "daemon:\n  interval_seconds: 1\ndorm_electricity:\n  campus: C&Campus\n  building: B&Building\n  floor: F&Floor\n  room: R&Room\nnotifications:\n  channels:\n    - type: webhook\n      id: test\n      url: invalid-url\n").unwrap();
    let settings = Settings::read(&config).unwrap();
    sdu_infohelper::auth::secure_write(
        &settings.cache_path(&config),
        br#"{"access_token":"fake-token","refresh_token":null,"expires_at":null,"client_authorization":null}"#,
    ).unwrap();
    with_dorm_auth(&config, Duration::from_secs(1), |request, _| {
        assert_eq!(request.location()?.room, "R&Room");
        Ok(())
    })
    .unwrap();
}

#[test]
fn daemon_still_rejects_invalid_monitoring_or_notification_settings() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    for content in [
        "daemon:\n  interval_seconds: 1\nnotifications:\n  channels:\n    - type: webhook\n      id: test\n      url: http://127.0.0.1:1/test\n",
        "notifications:\n  channels:\n    - type: webhook\n      id: test\n      url: invalid-url\n",
    ] {
        fs::write(&config, content).unwrap();
        let output = cli(&config, &["daemon", "run"]);
        assert!(!output.status.success(), "{output:?}");
        assert!(
            !dir.path()
                .join(".local/electricity/history.sqlite3")
                .exists()
        );
    }
}

#[test]
fn explicit_notification_test_ignores_unrelated_monitoring_errors() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let (url, requests, server) = common::server("204 No Content", "", "", Duration::ZERO);
    fs::write(&config, format!("daemon:\n  threshold_kwh: '-1'\nnotifications:\n  channels:\n    - type: webhook\n      id: test\n      url: {url}\n")).unwrap();
    let result = cli(&config, &["daemon", "test-notification"]);
    assert!(result.status.success(), "{result:?}");
    let request = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["event"], "electricity.test");
    assert!(!request.headers.to_lowercase().contains("cookie:"));
    server.join().unwrap();
    assert!(!dir.path().join(".local").exists());
}
