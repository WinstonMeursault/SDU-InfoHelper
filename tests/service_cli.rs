use sdu_infohelper::auth;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn trusted_stdio_requests_are_scoped_and_cannot_smuggle_other_user_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let config = dir.path().join("input.yaml");
    auth::secure_write(&config,b"cas:\n  username: fake-account\n  password: fake-secret\ndorm_electricity:\n  building: B&Building\n  floor: F&Floor\n  room: R&Room\n").unwrap();
    let bind = Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"))
        .args(["service", "--data-dir"])
        .arg(&root)
        .args(["bind", "--bot", "90001", "--qq", "10001", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(
        bind.status.success(),
        "{}",
        String::from_utf8_lossy(&bind.stderr)
    );
    let output: String = String::from_utf8(bind.stdout).unwrap();
    assert!(!output.contains("fake-secret"));
    let requests = [
        json!({"request_id":"a","bot_id":"90001","user_id":"10001","request":{"action":"preferences","patch":{"threshold_kwh":"6.5"}}}),
        json!({"request_id":"b","bot_id":"90001","user_id":"10002","request":{"action":"status"}}),
        json!({"request_id":"c","bot_id":"90001","user_id":"10001","request":{"action":"status"}}),
        json!({"request_id":"d","bot_id":"90001","user_id":"10001","request":{"action":"query","config":"/tmp/other-user.yaml"}}),
        json!({"request_id":"e","bot_id":"90002","user_id":"10001","request":{"action":"status"}}),
        json!({"request_id":"f","bot_id":"90001","user_id":"10001","request":{"action":"unbind"}}),
        json!({"request_id":"g","bot_id":"90001","user_id":"10001","request":{"action":"status"}}),
    ];
    let mut child = Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"))
        .args(["service", "--data-dir"])
        .arg(&root)
        .arg("stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let input = child.stdin.as_mut().unwrap();
        for request in requests {
            writeln!(input, "{request}").unwrap();
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    let replies: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 7);
    assert_eq!(replies[0]["data"]["preferences"]["threshold_kwh"], "6.5");
    assert_eq!(replies[1]["error_code"], "not_bound");
    assert_eq!(replies[2]["data"]["preferences"]["threshold_kwh"], "6.5");
    assert_eq!(replies[3]["error_code"], "invalid_request");
    assert_eq!(replies[4]["error_code"], "not_bound");
    assert_eq!(replies[5]["data"]["unbound"], true);
    assert_eq!(replies[6]["error_code"], "not_bound");
    for forbidden in ["fake-secret", "fake-account", "/tmp/other-user.yaml"] {
        assert!(!text.contains(forbidden));
    }
    assert_eq!(fs::read_dir(root.join("users")).unwrap().count(), 0);
}
