use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Output {
    pub success: bool,
    pub stdout: String,
}

pub(super) trait Runner {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, String>;
}

pub(super) struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if program == "systemctl" {
            // Project scripts use an isolated XDG config directory for build tooling.
            // Native units live in the current user's standard systemd directory.
            command.env_remove("XDG_CONFIG_HOME");
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW for management helpers only.
        }
        let mut child = command.spawn().map_err(|_| "无法启动系统服务管理工具。")?;
        let stdout = child.stdout.take().ok_or("无法读取服务管理器输出。")?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(1_048_577)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + Duration::from_secs(45);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err("系统服务管理操作超时或失败。".into());
                }
            }
        };
        let bytes = reader
            .join()
            .map_err(|_| "无法读取服务管理器输出。")?
            .map_err(|_| "无法读取服务管理器输出。")?;
        if bytes.len() > 1_048_576 {
            return Err("服务管理器输出过大。".into());
        }
        // Helpers emit JSON or ASCII properties; Windows PowerShell may include a UTF-8 BOM.
        // schtasks uses the OEM code page for human-readable status messages.
        // Structured PowerShell output explicitly uses UTF-8; action messages are ignored.
        let stdout = String::from_utf8_lossy(&bytes);
        Ok(Output {
            success: status.success(),
            stdout: stdout.trim_start_matches('\u{feff}').trim().into(),
        })
    }
}

pub(super) fn checked(
    runner: &dyn Runner,
    program: &str,
    args: &[String],
) -> Result<Output, String> {
    let output = runner.run(program, args)?;
    if !output.success {
        return Err("系统服务管理操作失败；请检查当前用户的服务管理器权限和可用性。".into());
    }
    Ok(output)
}
