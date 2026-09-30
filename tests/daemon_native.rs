//! Opt-in: creates and removes only a unique service for this temporary configuration.
use sdu_infohelper::daemon::Paths;
use std::{
    fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn cli(config: &Path, args: &[&str]) -> std::process::Output {
    Command::new(test_binary(config))
        .env_remove("SDU_INFOHELPER_HISTORY")
        .env_remove("SDU_INFOHELPER_CONFIG")
        .args(["daemon", "--config"])
        .arg(config)
        .args(args)
        .output()
        .unwrap()
}
fn test_binary(config: &Path) -> std::path::PathBuf {
    config
        .parent()
        .unwrap()
        .join("程序 binary $ % '")
        .join(if cfg!(windows) {
            "sdu-infohelper.exe"
        } else {
            "sdu-infohelper"
        })
}
fn ok(config: &Path, args: &[&str]) {
    let output = cli(config, args);
    assert!(output.status.success(), "{args:?}: {output:?}");
}
fn status(config: &Path) -> serde_json::Value {
    let output = cli(config, &["status", "--json"]);
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
struct Cleanup<'a>(&'a Path);
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = cli(self.0, &["uninstall", "--wait-seconds", "5"]);
    }
}
fn wait(paths: &Paths, previous: Option<&str>) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(state) = paths.state().unwrap()
            && state.status == "needs_login"
            && previous != Some(state.run_id.as_str())
        {
            return state.run_id;
        }
        assert!(Instant::now() < deadline, "native worker did not start");
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn assert_no_console(pid: u32) {
    use std::os::windows::process::CommandExt;
    let script = r#"Add-Type -TypeDefinition 'using System.Runtime.InteropServices; public static class NativeConsole { [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AttachConsole(uint id); [DllImport("kernel32.dll")] public static extern bool FreeConsole(); }';
[void][NativeConsole]::FreeConsole();
if ([NativeConsole]::AttachConsole(WORKER_PID)) { [void][NativeConsole]::FreeConsole(); exit 1 };
if ([Runtime.InteropServices.Marshal]::GetLastWin32Error() -ne 6) { exit 2 }; exit 0"#;
    let output = Command::new("powershell.exe")
        .creation_flags(0x08000000)
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(script.replace("WORKER_PID", &pid.to_string()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "worker must have no console: {output:?}"
    );
}

#[test]
#[ignore = "requires a real current-user systemd, launchd GUI, or Windows scheduler session"]
fn native_install_start_restart_autostart_stop_uninstall() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("中文 config $ % ' space");
    fs::create_dir(&folder).unwrap();
    let config = folder.join("config.yaml");
    let binary = test_binary(&config);
    fs::create_dir(binary.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_sdu-infohelper"), &binary).unwrap();
    // A closed loopback port provides deterministic delivery failure without real credentials.
    fs::write(&config, "daemon:\n  interval_seconds: 60\nnotifications:\n  channels:\n    - type: webhook\n      id: native-test\n      url: http://127.0.0.1:1/test\n      timeout_seconds: 1\n").unwrap();
    let _cleanup = Cleanup(&config);
    let paths = Paths::new(&config).unwrap();
    ok(&config, &["install"]);
    let s = status(&config);
    assert_eq!(s["running"], false);
    assert_eq!(s["service"]["installed"], true);
    assert_eq!(s["service"]["autostart"], false);
    ok(&config, &["start"]);
    let first = wait(&paths, None);
    #[cfg(windows)]
    assert_no_console(paths.state().unwrap().unwrap().pid);
    assert_eq!(status(&config)["service"]["active"], true);
    ok(&config, &["start"]);
    assert_eq!(paths.state().unwrap().unwrap().run_id, first);
    ok(&config, &["restart", "--wait-seconds", "5"]);
    let second = wait(&paths, Some(&first));
    assert_ne!(first, second);
    ok(&config, &["stop", "--wait-seconds", "5"]);
    assert_eq!(status(&config)["running"], false);
    assert_eq!(status(&config)["service"]["active"], false);
    ok(&config, &["install", "--autostart"]);
    assert_eq!(status(&config)["service"]["autostart"], true);
    assert!(!paths.running().unwrap());
    ok(&config, &["install"]);
    assert_eq!(status(&config)["service"]["autostart"], false);
    ok(&config, &["start"]);
    wait(&paths, Some(&second));
    ok(&config, &["uninstall", "--wait-seconds", "5"]);
    assert_eq!(status(&config)["service"]["registered"], false);
    assert!(!paths.running().unwrap());
    assert!(folder.join(".local/electricity/history.sqlite3").is_file());
    assert!(paths.file("daemon.log").is_file());
    assert!(config.is_file());
    ok(&config, &["uninstall"]);
    ok(&config, &["stop"]);
}
