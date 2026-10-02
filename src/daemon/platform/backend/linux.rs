//! systemd user-manager operations.
use super::super::{
    Registration,
    command::{Output, Runner, checked},
};
use super::{ManagerState, remove};
use std::collections::BTreeMap;

pub(super) fn available(runner: &dyn Runner) -> Result<Output, String> {
    runner.run(
        "systemctl",
        &[
            "--user".into(),
            "show".into(),
            "--property=Version".into(),
            "--value".into(),
        ],
    )
}

pub(super) fn install(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
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

pub(super) fn start(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    // Inactive units can be garbage-collected before reset-failed, which then
    // reports "not loaded". Starting is authoritative and reloads the unit.
    let _ = systemctl(reg, runner, "reset-failed");
    systemctl(reg, runner, "start")
}

pub(super) fn stop(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    systemctl(reg, runner, "stop")
}

pub(super) fn uninstall(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    if inspect(reg, runner)?.installed {
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

pub(super) fn inspect(reg: &Registration, runner: &dyn Runner) -> Result<ManagerState, String> {
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
    Ok(ManagerState {
        installed: *load != "not-found",
        active: matches!(*active, "active" | "activating" | "deactivating"),
        autostart: matches!(enabled, "enabled" | "enabled-runtime"),
    })
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
