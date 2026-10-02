use super::super::command::Output;
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
};

#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<(String, Vec<String>)>>,
    running: Cell<bool>,
    installed: Cell<bool>,
    fail: Cell<bool>,
}
impl Runner for Fake {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, String> {
        self.calls
            .borrow_mut()
            .push((program.into(), args.to_vec()));
        if self.fail.get() {
            return Ok(Output {
                success: false,
                stdout: "fake-secret".into(),
            });
        }
        let mut success = true;
        let stdout = if program == "powershell.exe" {
            let bytes = STANDARD.decode(args.last().unwrap()).unwrap();
            let units: Vec<_> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect();
            let script = String::from_utf16(&units).unwrap();
            if script.contains("WindowsIdentity") {
                "S-1-5-21-123".into()
            } else if script.contains("GetTasks(1)") {
                format!(
                    "{{\"installed\":{},\"active\":{},\"autostart\":false}}",
                    self.installed.get(),
                    self.running.get()
                )
            } else {
                "ready".into()
            }
        } else if program == "launchctl" {
            if args[0] == "bootstrap" {
                self.running.set(true);
            }
            if args[0] == "bootout" {
                self.running.set(false);
            }
            if args[0] == "print" {
                success = self.running.get();
            }
            "state = running".into()
        } else if program == "systemctl" {
            if args.iter().any(|v| v == "start") {
                self.running.set(true);
            }
            if args.iter().any(|v| v == "stop") {
                self.running.set(false);
            }
            if args.iter().any(|v| v.starts_with("--property=LoadState")) {
                format!(
                    "LoadState=loaded\nActiveState={}\nUnitFileState=disabled",
                    if self.running.get() {
                        "active"
                    } else {
                        "inactive"
                    }
                )
            } else {
                "255".into()
            }
        } else {
            match args[0].as_str() {
                "/Create" => self.installed.set(true),
                "/Delete" => self.installed.set(false),
                "/Run" => self.running.set(true),
                "/End" => self.running.set(false),
                _ => {}
            }
            String::new()
        };
        Ok(Output { success, stdout })
    }
}
fn reg(platform: Platform, definition: PathBuf) -> Registration {
    Registration {
        version: 1,
        platform,
        name: "test-service".into(),
        binary: "/tmp/test-bin".into(),
        working_directory: "/tmp".into(),
        arguments: Vec::new(),
        autostart: false,
        context: if platform == Platform::Macos {
            "gui/501"
        } else {
            "S-1-5-21-123"
        }
        .into(),
        definition,
    }
}
#[test]
fn all_backends_install_start_inspect_stop_and_uninstall_without_other_services() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, "{}\n").unwrap();
        let paths = Paths::new(&config).unwrap();
        let reg = reg(platform, dir.path().join("test.definition"));
        fs::write(&reg.definition, "owned test file").unwrap();
        let fake = Fake::default();
        available(&reg, &fake).unwrap();
        install(&reg, &fake).unwrap();
        assert_eq!(inspect(&reg, &fake).unwrap().active, Some(false));
        start(&reg, &fake).unwrap();
        assert_eq!(inspect(&reg, &fake).unwrap().active, Some(true));
        stop(&reg, &paths, &fake).unwrap();
        assert_eq!(inspect(&reg, &fake).unwrap().active, Some(false));
        uninstall(&reg, &fake).unwrap();
        assert!(!reg.definition.exists());
        if platform == Platform::Macos {
            assert!(
                !fake
                    .calls
                    .borrow()
                    .iter()
                    .any(|(_, args)| args[0] == "disable")
            );
        }
        assert!(
            fake.calls
                .borrow()
                .iter()
                .filter(|(program, _)| program != "powershell.exe")
                .all(|(_, args)| args.iter().all(|arg| !arg.contains("fake-secret")))
        );
    }
}
#[test]
fn unavailable_manager_errors_never_echo_helper_output() {
    for platform in [Platform::Linux, Platform::Macos, Platform::Windows] {
        let fake = Fake::default();
        fake.fail.set(true);
        let error = available(&reg(platform, PathBuf::from("test")), &fake).unwrap_err();
        assert!(error.contains("daemon run"));
        assert!(!error.contains("fake-secret"));
    }
}
#[test]
fn windows_context_is_sid_and_control_char_paths_are_rejected() {
    assert_eq!(
        context(Platform::Windows, &Fake::default()).unwrap(),
        "S-1-5-21-123"
    );
    assert!(path(Path::new("/tmp/bad\npath")).is_err());
    assert_eq!(path(Path::new("/tmp/中文 path")).unwrap(), "/tmp/中文 path");
}
