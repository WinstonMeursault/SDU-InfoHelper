//! Current-user task scheduler and verified worker termination.
use super::super::{
    Registration,
    command::{Output, Runner, checked},
    render::{encoded, powershell_literal},
};
use super::{ManagerState, path, remove};
use crate::daemon::{Paths, RuntimeState};

pub(super) fn context(runner: &dyn Runner) -> Result<String, String> {
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

pub(super) fn available(runner: &dyn Runner) -> Result<Output, String> {
    ps(
        runner,
        "$s=New-Object -ComObject Schedule.Service; $s.Connect(); $null=$s.GetFolder('\\'); Write-Output 'ready'",
    )
}

pub(super) fn install(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
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

pub(super) fn start(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    checked(
        runner,
        "schtasks.exe",
        &["/Run".into(), "/TN".into(), reg.name.clone()],
    )?;
    Ok(())
}

pub(super) fn stop(reg: &Registration, paths: &Paths, runner: &dyn Runner) -> Result<(), String> {
    if inspect(reg, runner)?.active {
        let result = runner.run(
            "schtasks.exe",
            &["/End".into(), "/TN".into(), reg.name.clone()],
        )?;
        if !result.success && inspect(reg, runner)?.active {
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

pub(super) fn uninstall(reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    if inspect(reg, runner)?.installed {
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

pub(super) fn inspect(reg: &Registration, runner: &dyn Runner) -> Result<ManagerState, String> {
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
    Ok(ManagerState {
        installed: field("installed")?,
        active: field("active")?,
        autostart: field("autostart")?,
    })
}

fn ps(runner: &dyn Runner, script: &str) -> Result<Output, String> {
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
