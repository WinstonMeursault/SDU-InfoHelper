use super::*;
use crate::settings::Settings;
use rust_decimal::Decimal;
use std::{fs, thread, time::Duration};

#[test]
fn stop_waits_for_new_state_and_status_never_reports_stale_live_pid() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "{}\n").unwrap();
    let paths = Paths::new(&config).unwrap();
    let lock = paths.acquire().unwrap();
    let mut state = RuntimeState {
        run_id: "old".into(),
        pid: 999999,
        executable: None,
        status: "healthy".into(),
        started_at: "old".into(),
        last_attempt_at: None,
        last_success: None,
        next_check_at: None,
        last_error: None,
        channels: Vec::new(),
        recent_starts: Vec::new(),
    };
    paths.save(&state).unwrap();
    let status = local_status(&config).unwrap();
    assert!(status.running);
    assert!(status.runtime.is_none());
    let stop_paths = paths.clone();
    let stopper = thread::spawn(move || stop_paths.request_stop());
    thread::sleep(Duration::from_millis(50));
    state.run_id.clone_from(&lock.run_id);
    paths.save(&state).unwrap();
    assert!(stopper.join().unwrap().unwrap());
    assert!(paths.stop_requested(&lock.run_id).unwrap());
}

#[test]
fn canonical_config_identity_and_locks_are_independent_of_history() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("中文 path");
    fs::create_dir(&folder).unwrap();
    let config = folder.join("config.yaml");
    fs::write(&config, "{}\n").unwrap();
    let paths = Paths::new(&config).unwrap();
    assert!(!paths.running().unwrap());
    let lock = paths.acquire().unwrap();
    assert!(paths.running().unwrap());
    assert!(Paths::new(&config).unwrap().acquire().is_err());
    assert_eq!(
        Paths::new(&folder.join("./config.yaml")).unwrap().instance,
        paths.instance
    );
    drop(lock);
    assert!(!paths.running().unwrap());
    fs::remove_file(&config).unwrap();
    assert_eq!(Paths::new(&config).unwrap().instance, paths.instance);
}

#[test]
fn yaml_paths_resolve_against_config_and_explicit_paths_are_absolute() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let settings = Settings::default();
    let options = RunOptions {
        history: Some(dir.path().join("override.sqlite3")),
        threshold: Some(Decimal::ZERO),
        ..RunOptions::default()
    };
    let resolved = options.resolve(&settings, &config).unwrap();
    assert_eq!(resolved.history, dir.path().join("override.sqlite3"));
    assert_eq!(resolved.threshold_kwh, Decimal::ZERO);
    // The normal default is tested in CLI children with the history env var removed.
}

#[test]
fn rotation_is_bounded_and_old_stop_requests_do_not_match_new_instances() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "{}\n").unwrap();
    let paths = Paths::new(&config).unwrap();
    paths.ensure().unwrap();
    let log = Log {
        paths: paths.clone(),
        max_bytes: 100,
    };
    for _ in 0..10 {
        log.write(&"x".repeat(60)).unwrap();
    }
    for index in 1..=3 {
        assert!(paths.file(&format!("daemon.log.{index}")).exists());
    }
    assert!(!paths.file("daemon.log.4").exists());
    fs::write(paths.file("stop.request"), "old").unwrap();
    assert!(!paths.stop_requested("new").unwrap());
    assert!(paths.stop_requested("old").unwrap());
}
