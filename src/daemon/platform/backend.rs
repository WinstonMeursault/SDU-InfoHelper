use super::{
    Platform, Registration, ServiceStatus,
    command::{Runner, checked},
    render::{encoded, powershell_literal},
};
use crate::daemon::{Paths, RuntimeState};
use std::{collections::BTreeMap, fs, path::Path};

pub(super) fn ps(runner: &dyn Runner, script: &str) -> Result<super::command::Output, String> {
    let script = format!(
        "$ErrorActionPreference='Stop'; [Console]::OutputEncoding=New-Object System.Text.UTF8Encoding; {script}"
    );
    runner.run(
        "powershell.exe",
        &[
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            encoded(&script),
        ],
    )
}

pub(super) fn context(platform: Platform, runner: &dyn Runner) -> Result<String, String> {
    match platform {
        Platform::Linux => Ok(String::new()),
        Platform::Macos => {
            #[cfg(unix)]
            {
                // SAFETY: geteuid only reads this process's effective UID.
                Ok(format!("gui/{}", unsafe { libc::geteuid() }))
            }
            #[cfg(not(unix))]
            {
                Err("该系统不支持 launchd。".into())
            }
        }
        Platform::Windows => {
            let output = ps(
                runner,
                "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            )?;
            if !output.success
                || !output.stdout.starts_with("S-1-")
                || !output
                    .stdout
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, 'S' | '-'))
            {
                return Err("无法读取当前 Windows 用户标识。".into());
            }
            Ok(output.stdout)
        }
    }
}

pub(super) fn available(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    let result = match reg.platform {
        Platform::Linux => runner.run(
            "systemctl",
            &[
                "--user".into(),
                "show".into(),
                "--property=Version".into(),
                "--value".into(),
            ],
        ),
        Platform::Macos => runner.run("launchctl", &["print-disabled".into(), reg.context.clone()]),
        Platform::Windows => ps(
            runner,
            "$s=New-Object -ComObject Schedule.Service; $s.Connect(); $null=$s.GetFolder('\\'); Write-Output 'ready'",
        ),
    };
    if !result.is_ok_and(|output| output.success) {
        return Err(match reg.platform {
            Platform::Linux => "当前用户的 systemd 不可用；可使用 daemon run。",
            Platform::Macos => "当前用户的 launchd 图形登录会话不可用；可使用 daemon run。",
            Platform::Windows => "当前用户的任务计划程序不可用；可使用 daemon run。",
        }
        .into());
    }
    Ok(())
}

fn systemctl(reg: &Registration, runner: &dyn Runner, action: &str) -> Result<(), String> {
    checked(
        runner,
        "systemctl",
        &[
            "--user".into(),
            "--no-pager".into(),
            action.into(),
            reg.name.clone(),
        ],
    )?;
    Ok(())
}

pub(super) fn install(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => {
            checked(
                runner,
                "systemctl",
                &["--user".into(), "daemon-reload".into()],
            )?;
            systemctl(
                reg,
                runner,
                if reg.autostart { "enable" } else { "disable" },
            )
        }
        Platform::Macos => Ok(()), // Installed plist is loaded on start or the next login.
        Platform::Windows => {
            checked(
                runner,
                "schtasks.exe",
                &[
                    "/Create".into(),
                    "/TN".into(),
                    reg.name.clone(),
                    "/XML".into(),
                    path(&reg.definition)?,
                    "/F".into(),
                ],
            )?;
            Ok(())
        }
    }
}

fn mac_loaded(reg: &Registration, runner: &dyn Runner) -> Result<bool, String> {
    Ok(runner
        .run(
            "launchctl",
            &["print".into(), format!("{}/{}", reg.context, reg.name)],
        )?
        .success)
}

pub(super) fn start(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => {
            // Inactive units can be garbage-collected before reset-failed, which then
            // reports "not loaded". Starting is authoritative and reloads the unit.
            let _ = systemctl(reg, runner, "reset-failed");
            systemctl(reg, runner, "start")
        }
        Platform::Macos => {
            // A prior explicit stop may leave the job marked disabled in launchd.
            checked(
                runner,
                "launchctl",
                &["enable".into(), format!("{}/{}", reg.context, reg.name)],
            )?;
            if mac_loaded(reg, runner)? {
                checked(
                    runner,
                    "launchctl",
                    &["bootout".into(), format!("{}/{}", reg.context, reg.name)],
                )?;
            }
            checked(
                runner,
                "launchctl",
                &[
                    "bootstrap".into(),
                    reg.context.clone(),
                    path(&reg.definition)?,
                ],
            )?;
            Ok(())
        }
        Platform::Windows => {
            checked(
                runner,
                "schtasks.exe",
                &["/Run".into(), "/TN".into(), reg.name.clone()],
            )?;
            Ok(())
        }
    }
}

pub(super) fn stop(reg: &Registration, paths: &Paths, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => systemctl(reg, runner, "stop"),
        Platform::Macos => {
            if mac_loaded(reg, runner)? {
                checked(
                    runner,
                    "launchctl",
                    &["bootout".into(), format!("{}/{}", reg.context, reg.name)],
                )?;
            }
            Ok(())
        }
        Platform::Windows => {
            if inspect(reg, runner)?.active == Some(true) {
                let result = runner.run(
                    "schtasks.exe",
                    &["/End".into(), "/TN".into(), reg.name.clone()],
                )?;
                if !result.success && inspect(reg, runner)?.active != Some(false) {
                    return Err("无法停止 Windows 后台任务。".into());
                }
            }
            // Ending the scheduler's PowerShell launcher can leave its worker alive.
            // Verify the current run token and executable before touching that worker.
            if paths.running()? {
                let state = paths.state()?.ok_or("无法核验后台工作进程。")?;
                if !paths.owner_matches(&state.run_id)? {
                    return Err("后台实例已变更，请重新停止。".into());
                }
                terminate_worker(reg, &state, runner)?;
            }
            Ok(())
        }
    }
}

fn terminate_worker(
    reg: &Registration,
    state: &RuntimeState,
    runner: &dyn Runner,
) -> Result<(), String> {
    let executable = state
        .executable
        .as_ref()
        .and_then(|path| path.to_str())
        .unwrap_or(&reg.binary);
    let binary = executable.strip_prefix("\\\\?\\").unwrap_or(executable);
    let script = format!(
        "$p=$null; try {{ $p=[System.Diagnostics.Process]::GetProcessById({}); $null=$p.Handle; \
        $w=Get-CimInstance Win32_Process -Filter 'ProcessId={}'; if($null -eq $w){{exit 0}}; \
        if($w.ExecutablePath -ne {} -or !$w.CommandLine.Contains({})){{exit 3}}; $p.Kill(); $p.WaitForExit(); \
        }} catch {{ if($null -ne $p -and $p.HasExited){{exit 0}}; exit 4 }} finally {{if($null -ne $p){{$p.Dispose()}}}}",
        state.pid,
        state.pid,
        powershell_literal(binary),
        powershell_literal(&format!("--run-id {}", state.run_id))
    );
    let output = ps(runner, &script)?;
    if !output.success {
        return Err("无法核验并停止 Windows 工作进程；请等待网络操作超时后重试。".into());
    }
    Ok(())
}

pub(super) fn uninstall(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => {
            if inspect(reg, runner)?.installed == Some(true) {
                systemctl(reg, runner, "disable")?;
            }
            remove(&reg.definition)?;
            checked(
                runner,
                "systemctl",
                &["--user".into(), "daemon-reload".into()],
            )?;
            Ok(())
        }
        Platform::Macos => remove(&reg.definition),
        Platform::Windows => {
            if inspect(reg, runner)?.installed == Some(true) {
                checked(
                    runner,
                    "schtasks.exe",
                    &[
                        "/Delete".into(),
                        "/TN".into(),
                        reg.name.clone(),
                        "/F".into(),
                    ],
                )?;
            }
            remove(&reg.definition)
        }
    }
}

pub(super) fn inspect(reg: &Registration, runner: &dyn Runner) -> Result<ServiceStatus, String> {
    let (installed, active, autostart) = match reg.platform {
        Platform::Linux => {
            let output = runner.run(
                "systemctl",
                &[
                    "--user".into(),
                    "show".into(),
                    reg.name.clone(),
                    "--property=LoadState,ActiveState,UnitFileState".into(),
                ],
            )?;
            let values: BTreeMap<_, _> = output
                .stdout
                .lines()
                .filter_map(|line| line.split_once('='))
                .collect();
            let load = values.get("LoadState").ok_or("systemd 状态响应不完整。")?;
            let active = values
                .get("ActiveState")
                .ok_or("systemd 状态响应不完整。")?;
            let enabled = values.get("UnitFileState").copied().unwrap_or("");
            (
                *load != "not-found",
                matches!(*active, "active" | "activating" | "deactivating"),
                matches!(enabled, "enabled" | "enabled-runtime"),
            )
        }
        Platform::Macos => {
            let output = runner.run(
                "launchctl",
                &["print".into(), format!("{}/{}", reg.context, reg.name)],
            )?;
            let active = if output.success {
                output
                    .stdout
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("state = "))
                    .map(|state| state == "running")
                    .ok_or("launchd 状态响应不完整。")?
            } else {
                false
            };
            (reg.definition.is_file(), active, reg.autostart)
        }
        Platform::Windows => {
            let script = format!(
                "$s=New-Object -ComObject Schedule.Service; $s.Connect(); \
                $t=@($s.GetFolder('\\').GetTasks(1) | Where-Object {{$_.Name -eq {}}}) | Select-Object -First 1; \
                if($null -eq $t){{@{{installed=$false;active=$false;autostart=$false}} | ConvertTo-Json -Compress}} \
                else {{@{{installed=$true;active=($t.State -eq 4);autostart=($t.Enabled -and $t.Definition.Triggers.Count -gt 0)}} | ConvertTo-Json -Compress}}",
                powershell_literal(&reg.name)
            );
            let output = ps(runner, &script)?;
            if !output.success {
                return Err("无法查询 Windows 任务状态。".into());
            }
            let payload: serde_json::Value =
                serde_json::from_str(&output.stdout).map_err(|_| "Windows 任务状态格式无效。")?;
            let field = |key: &str| {
                payload
                    .get(key)
                    .and_then(serde_json::Value::as_bool)
                    .ok_or("Windows 任务状态字段缺失。".to_owned())
            };
            (field("installed")?, field("active")?, field("autostart")?)
        }
    };
    Ok(ServiceStatus {
        registered: true,
        available: Some(true),
        installed: Some(installed),
        active: Some(active),
        autostart: Some(autostart),
        name: Some(reg.name.clone()),
        error: None,
    })
}

pub(super) fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("无法移除本程序的服务定义。".into()),
    }
}

pub(super) fn path(path: &Path) -> Result<String, String> {
    let value = path.to_str().ok_or("服务路径必须可以表示为 Unicode。")?;
    if value.chars().any(char::is_control) {
        return Err("后台服务路径不允许控制字符。".into());
    }
    Ok(value.into())
}

#[cfg(test)]
mod tests {
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
}
