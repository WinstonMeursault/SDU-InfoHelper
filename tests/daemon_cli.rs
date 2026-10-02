mod common;

use common::server;
use sdu_infohelper::daemon::Paths;
use std::{
    fs,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cli(config: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"));
    command
        .env_remove("SDU_INFOHELPER_HISTORY")
        .env_remove("SDU_INFOHELPER_CONFIG")
        .arg("daemon")
        .arg("--config")
        .arg(config);
    command
}

fn start(config: &std::path::Path, cwd: &std::path::Path) -> Worker {
    Worker(
        cli(config)
            .arg("run")
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

fn wait_state(
    paths: &Paths,
    predicate: impl Fn(&sdu_infohelper::daemon::RuntimeState) -> bool,
) -> sdu_infohelper::daemon::RuntimeState {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(state) = paths.state().unwrap()
            && predicate(&state)
        {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "worker failed to reach expected state; last state: {}",
            serde_json::to_string(&paths.state().unwrap()).unwrap()
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn finish(worker: &mut Worker) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = worker.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "worker failed to stop");
        thread::sleep(Duration::from_millis(25));
    }
}

fn configure(folder: &std::path::Path, url: &str) -> std::path::PathBuf {
    let config = folder.join("config.yaml");
    fs::write(&config, format!("daemon:\n  interval_seconds: 60\nnotifications:\n  channels:\n    - type: webhook\n      id: w\n      url: '{url}'\n      headers:\n        Authorization: Bearer fake-secret\n")).unwrap();
    config
}

#[test]
fn worker_reports_login_keeps_running_locks_instance_and_stops_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("中文 config folder");
    fs::create_dir(&folder).unwrap();
    let (url, requests, server) = server("204 No Content", "", "", Duration::ZERO);
    let config = configure(&folder, &url);
    let paths = Paths::new(&config).unwrap();
    let mut worker = start(&config, dir.path());
    let state = wait_state(&paths, |state| {
        state.status == "needs_login"
            && state
                .channels
                .iter()
                .any(|channel| channel.status == "accepted")
    });
    let request = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["event"], "electricity.auth_required");
    assert!(body["remaining_kwh"].is_null());
    assert!(!request.headers.to_lowercase().contains("cookie:"));
    server.join().unwrap();
    assert!(paths.running().unwrap());
    assert!(folder.join(".local/electricity/history.sqlite3").exists());
    assert!(
        !dir.path()
            .join(".local/electricity/history.sqlite3")
            .exists()
    );
    let duplicate = cli(&config).arg("run").output().unwrap();
    assert!(!duplicate.status.success());
    let output = cli(&config).args(["status", "--json"]).output().unwrap();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["running"], true);
    assert_eq!(status["runtime"]["status"], "needs_login");
    assert!(
        !String::from_utf8(output.stdout)
            .unwrap()
            .contains("fake-secret")
    );
    assert!(
        !fs::read_to_string(paths.file("daemon.log"))
            .unwrap()
            .contains("fake-secret")
    );
    let output = cli(&config)
        .args(["stop", "--wait-seconds", "5"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    finish(&mut worker);
    assert!(!paths.running().unwrap());
    assert_eq!(paths.state().unwrap().unwrap().status, "stopped");
    let mut next = start(&config, dir.path());
    wait_state(&paths, |new| {
        new.run_id != state.run_id && new.status == "needs_login"
    });
    thread::sleep(Duration::from_millis(300));
    assert!(
        next.0.try_wait().unwrap().is_none(),
        "old stop request stopped new instance"
    );
    let output = cli(&config)
        .args(["stop", "--wait-seconds", "5"])
        .output()
        .unwrap();
    assert!(output.status.success());
    finish(&mut next);
    let connection =
        rusqlite::Connection::open(folder.join(".local/electricity/history.sqlite3")).unwrap();
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM readings WHERE remaining_kwh IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn notification_test_is_explicit_and_does_not_create_history_or_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    let (url, requests, handle) = server("204 No Content", "", "", Duration::ZERO);
    let config = configure(dir.path(), &url);
    let output = cli(&config)
        .args(["test-notification", "--channel", "w"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let body: serde_json::Value =
        serde_json::from_slice(&requests.recv_timeout(Duration::from_secs(3)).unwrap().body)
            .unwrap();
    assert_eq!(body["event"], "electricity.test");
    handle.join().unwrap();
    assert!(!dir.path().join(".local").exists());
    assert!(
        !cli(&config)
            .args(["test-notification", "--channel", "unknown"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn status_is_read_only_and_failed_start_releases_lock_with_visible_error() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "{}\n").unwrap();
    let output = cli(&config).args(["status", "--json"]).output().unwrap();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["running"], false);
    assert!(status["runtime"].is_null());
    assert!(!dir.path().join(".local").exists());
    let output = cli(&config).arg("run").output().unwrap();
    assert!(!output.status.success());
    let paths = Paths::new(&config).unwrap();
    assert!(!paths.running().unwrap());
    assert_eq!(paths.state().unwrap().unwrap().status, "failed");
    assert!(
        cli(&config)
            .args(["run", "--managed"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(paths.state().unwrap().unwrap().status, "failed");
}

#[test]
fn stop_interrupts_authentication_lock_wait_without_waiting_for_the_lock_owner() {
    use fs2::FileExt;
    let dir = tempfile::tempdir().unwrap();
    let config = configure(dir.path(), "http://127.0.0.1:1/test");
    let paths = Paths::new(&config).unwrap();
    let cache = dir.path().join(".local/electricity");
    fs::create_dir_all(&cache).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cache.join("auth.lock"))
        .unwrap();
    FileExt::lock_exclusive(&lock).unwrap();
    let mut worker = start(&config, dir.path());
    wait_state(&paths, |state| state.status == "checking");
    let stopped = cli(&config)
        .args(["stop", "--wait-seconds", "5"])
        .output()
        .unwrap();
    assert!(stopped.status.success(), "{stopped:?}");
    // The authentication owner still holds its lock when the worker exits.
    finish(&mut worker);
    assert_eq!(paths.state().unwrap().unwrap().status, "stopped");
    let connection = rusqlite::Connection::open(cache.join("history.sqlite3")).unwrap();
    let count: u32 = connection
        .query_row("SELECT COUNT(*) FROM readings", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "cancellation must not create a failed reading or an authentication alert"
    );
    drop(lock);
}

#[cfg(unix)]
#[test]
fn sigterm_exits_cleanly_and_releases_instance_lock() {
    let dir = tempfile::tempdir().unwrap();
    let (url, requests, handle) = server("204 No Content", "", "", Duration::ZERO);
    let config = configure(dir.path(), &url);
    let paths = Paths::new(&config).unwrap();
    let mut worker = start(&config, dir.path());
    wait_state(&paths, |state| {
        state.status == "needs_login"
            && state
                .channels
                .iter()
                .any(|channel| channel.status == "accepted")
    });
    requests.recv_timeout(Duration::from_secs(3)).unwrap();
    handle.join().unwrap();
    // SAFETY: this PID belongs to the live child owned by Worker, which is not reaped yet.
    assert_eq!(
        unsafe { libc::kill(worker.0.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    finish(&mut worker);
    assert!(!paths.running().unwrap());
    assert_eq!(paths.state().unwrap().unwrap().status, "stopped");
}
