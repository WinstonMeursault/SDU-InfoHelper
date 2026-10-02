//! launchd operations within the current graphical user session.
use super::super::{
    Registration,
    command::{Output, Runner, checked},
};
use super::{ManagerState, path, remove};

pub(super) fn context() -> Result<String, String> {
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

pub(super) fn available(reg: &Registration, runner: &dyn Runner) -> Result<Output, String> {
    runner.run("launchctl", &["print-disabled".into(), reg.context.clone()])
}

pub(super) fn start(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
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

pub(super) fn stop(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    if mac_loaded(reg, runner)? {
        checked(
            runner,
            "launchctl",
            &["bootout".into(), format!("{}/{}", reg.context, reg.name)],
        )?;
    }
    Ok(())
}

pub(super) fn uninstall(reg: &Registration) -> Result<(), String> {
    remove(&reg.definition)
}

pub(super) fn inspect(reg: &Registration, runner: &dyn Runner) -> Result<ManagerState, String> {
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
    Ok(ManagerState {
        installed: reg.definition.is_file(),
        active,
        autostart: reg.autostart,
    })
}

fn mac_loaded(reg: &Registration, runner: &dyn Runner) -> Result<bool, String> {
    Ok(runner
        .run(
            "launchctl",
            &["print".into(), format!("{}/{}", reg.context, reg.name)],
        )?
        .success)
}
