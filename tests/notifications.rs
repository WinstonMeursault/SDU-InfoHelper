use sdu_infohelper::{
    notification::{Alert, HttpNotifier, Notifier},
    settings::Settings,
};
use std::time::Duration;
mod common;
use common::server;

fn notifier(yaml: &str) -> HttpNotifier {
    let settings: Settings = serde_yaml::from_str(yaml).unwrap();
    settings.notifications.validate().unwrap();
    HttpNotifier::new(&settings.notifications.channels[0]).unwrap()
}

#[test]
fn webhook_posts_json_with_custom_auth_and_accepts_empty_204() {
    let (url, requests, handle) = server("204 No Content", "", "", Duration::ZERO);
    let sender = notifier(&format!(
        "notifications:\n  channels:\n    - type: webhook\n      id: w\n      url: {url}/alerts\n      headers:\n        Authorization: Bearer test-secret\n"
    ));
    let alert = Alert::test();
    sender.send(&alert).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(request.headers.starts_with("POST /alerts HTTP/1.1"));
    assert!(
        request
            .headers
            .to_lowercase()
            .contains("content-type: application/json")
    );
    assert!(
        request
            .headers
            .to_lowercase()
            .contains("authorization: bearer test-secret")
    );
    assert!(!request.headers.to_lowercase().contains("cookie:"));
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["schema_version"], 1);
    assert_eq!(body["event"], "electricity.test");
    assert_eq!(body["event_id"], alert.event_id);
    assert!(body["remaining_kwh"].is_null());
    assert!(
        !String::from_utf8(request.body)
            .unwrap()
            .contains("test-secret")
    );
    handle.join().unwrap();
}

#[test]
fn pushdeer_posts_urlencoded_key_in_body_and_checks_device_result() {
    let (url, requests, handle) = server(
        "200 OK",
        "",
        r#"{"code":0,"content":{"result":["{\"counts\":1}"]}}"#,
        Duration::ZERO,
    );
    let sender = notifier(&format!(
        "notifications:\n  channels:\n    - {{type: pushdeer, id: p, endpoint: '{url}/selfhost/', pushkey: 'test&secret=中文'}}\n"
    ));
    sender.send(&Alert::test()).unwrap();
    let request = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(
        request
            .headers
            .starts_with("POST /selfhost/message/push HTTP/1.1")
    );
    assert!(!request.headers.contains("test&secret"));
    let encoded = String::from_utf8(request.body).unwrap();
    let url = reqwest::Url::parse(&format!("http://localhost/?{encoded}")).unwrap();
    let fields: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(fields["pushkey"], "test&secret=中文");
    assert_eq!(fields["text"], "SDU-InfoHelper 推送测试");
    assert_eq!(fields["type"], "text");
    handle.join().unwrap();
}

#[test]
fn failures_are_redacted_and_classified_for_retry() {
    for (status, retryable) in [
        ("401 Unauthorized", false),
        ("503 Unavailable", true),
        ("429 Too Many Requests", true),
        ("302 Found", false),
    ] {
        let (url, requests, handle) = server(
            status,
            "Retry-After: 7\r\nLocation: http://127.0.0.1:1/secret\r\n",
            "test-secret",
            Duration::ZERO,
        );
        let sender = notifier(&format!(
            "notifications:\n  channels:\n    - {{type: webhook, id: w, url: '{url}/test-secret'}}\n"
        ));
        let error = sender.send(&Alert::test()).unwrap_err();
        assert_eq!(error.retryable, retryable, "{status}");
        if status.starts_with("429") {
            assert_eq!(error.retry_after_seconds, Some(7));
        }
        assert!(!error.to_string().contains("test-secret"));
        assert!(!format!("{error:?}").contains(&url));
        requests.recv_timeout(Duration::from_secs(3)).unwrap();
        handle.join().unwrap();
    }
}

#[test]
fn pushdeer_rejects_business_failures_empty_results_and_large_responses() {
    for body in [
        r#"{"code":80501,"error":"test-secret"}"#.to_owned(),
        r#"{"code":0,"content":{"result":[]}}"#.to_owned(),
        r#"{"code":0,"content":{"result":["{\"counts\":0}"]}}"#.to_owned(),
        "x".repeat(65537),
    ] {
        let (url, requests, handle) = server("200 OK", "", &body, Duration::ZERO);
        let sender = notifier(&format!(
            "notifications:\n  channels:\n    - {{type: pushdeer, id: p, endpoint: '{url}', pushkey: test-secret}}\n"
        ));
        let error = sender.send(&Alert::test()).unwrap_err();
        assert!(!error.to_string().contains("test-secret"));
        requests.recv_timeout(Duration::from_secs(3)).unwrap();
        handle.join().unwrap();
    }
}

#[test]
fn notification_timeout_is_bounded_and_retryable() {
    let (url, requests, handle) = server("200 OK", "", "", Duration::from_millis(1200));
    let sender = notifier(&format!(
        "notifications:\n  channels:\n    - {{type: webhook, id: w, url: '{url}', timeout_seconds: 1}}\n"
    ));
    let error = sender.send(&Alert::test()).unwrap_err();
    assert!(error.retryable);
    assert_eq!(error.to_string(), "通知请求超时。");
    requests.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
}
