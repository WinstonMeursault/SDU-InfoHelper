//! Dispatch platform operations; orchestration remains in the service layer.
mod linux;
mod macos;
mod windows;

use super::{Platform, Registration, ServiceStatus, command::Runner};
use crate::daemon::Paths;
use std::{fs, path::Path};

struct ManagerState {
    installed: bool,
    active: bool,
    autostart: bool,
}

pub(super) fn context(platform: Platform, runner: &dyn Runner) -> Result<String, String> {
    match platform {
        Platform::Linux => Ok(String::new()),
        Platform::Macos => macos::context(),
        Platform::Windows => windows::context(runner),
    }
}

pub(super) fn available(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    let result = match reg.platform {
        Platform::Linux => linux::available(runner),
        Platform::Macos => macos::available(reg, runner),
        Platform::Windows => windows::available(runner),
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

pub(super) fn install(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => linux::install(reg, runner),
        Platform::Macos => Ok(()), // Installed plist is loaded on start or the next login.
        Platform::Windows => windows::install(reg, runner),
    }
}

pub(super) fn start(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => linux::start(reg, runner),
        Platform::Macos => macos::start(reg, runner),
        Platform::Windows => windows::start(reg, runner),
    }
}

pub(super) fn stop(reg: &Registration, paths: &Paths, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => linux::stop(reg, runner),
        Platform::Macos => macos::stop(reg, runner),
        Platform::Windows => windows::stop(reg, paths, runner),
    }
}

pub(super) fn uninstall(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    match reg.platform {
        Platform::Linux => linux::uninstall(reg, runner),
        Platform::Macos => macos::uninstall(reg),
        Platform::Windows => windows::uninstall(reg, runner),
    }
}

pub(super) fn inspect(reg: &Registration, runner: &dyn Runner) -> Result<ServiceStatus, String> {
    let state = match reg.platform {
        Platform::Linux => linux::inspect(reg, runner),
        Platform::Macos => macos::inspect(reg, runner),
        Platform::Windows => windows::inspect(reg, runner),
    }?;
    Ok(ServiceStatus {
        registered: true,
        available: Some(true),
        installed: Some(state.installed),
        active: Some(state.active),
        autostart: Some(state.autostart),
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
mod tests;
