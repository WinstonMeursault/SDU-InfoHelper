use super::{Platform, Registration};
use base64::{Engine, engine::general_purpose::STANDARD};

pub(super) fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd(value: &str) -> String {
    // systemd specifiers and ExecStart environment expansion are not shell quoting.
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

pub(super) fn powershell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub(super) fn encoded(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    STANDARD.encode(bytes)
}

fn windows_argument(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            out.push_str(&"\\".repeat(slashes * 2 + 1));
        } else {
            out.push_str(&"\\".repeat(slashes));
        }
        out.push(c);
        slashes = 0;
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}

pub(super) fn windows_launcher(reg: &Registration) -> String {
    let args = reg
        .arguments
        .iter()
        .map(|arg| windows_argument(arg))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "$ErrorActionPreference='Stop'; $pinfo=New-Object System.Diagnostics.ProcessStartInfo; \
        $pinfo.FileName={}; $pinfo.WorkingDirectory={}; $pinfo.UseShellExecute=$false; \
        $pinfo.CreateNoWindow=$true; $pinfo.Arguments={}+' --run-id '+[guid]::NewGuid().ToString('N'); \
        $p=[System.Diagnostics.Process]::Start($pinfo); $p.WaitForExit(); exit $p.ExitCode",
        powershell_literal(&reg.binary),
        powershell_literal(&reg.working_directory),
        powershell_literal(&args)
    )
}

pub(super) fn definition(reg: &Registration) -> Vec<u8> {
    match reg.platform {
        Platform::Linux => {
            // systemd rejects quotes/backslashes in the executable token even after
            // unquoting. env execs the binary directly and permits arbitrary path arguments.
            let mut args = vec![":/usr/bin/env".into(), "--".into(), systemd(&reg.binary)];
            args.extend(reg.arguments.iter().map(|arg| systemd(arg)));
            format!("[Unit]\nDescription=SDU-InfoHelper electricity daemon\nStartLimitIntervalSec=120\nStartLimitBurst=3\n\n[Service]\nType=exec\nExecStart={}\nWorkingDirectory={}\nRestart=on-failure\nRestartSec=30\nTimeoutStopSec=30\nUMask=0077\nStandardOutput=null\nStandardError=journal\n\n[Install]\nWantedBy=default.target\n", args.join(" "), reg.working_directory.replace('%', "%%")).into_bytes()
        }
        Platform::Macos => {
            let mut args = vec![format!("<string>{}</string>", xml(&reg.binary))];
            args.extend(
                reg.arguments
                    .iter()
                    .map(|arg| format!("<string>{}</string>", xml(arg))),
            );
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{}</array><key>WorkingDirectory</key><string>{}</string><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>30</integer><key>ExitTimeOut</key><integer>30</integer><key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string></dict></plist>\n", xml(&reg.name), args.join(""), xml(&reg.working_directory)).into_bytes()
        }
        Platform::Windows => {
            // TaskSettings.RestartInterval requires at least one minute.
            // https://learn.microsoft.com/windows/win32/taskschd/tasksettings-restartinterval
            let trigger = if reg.autostart {
                format!(
                    "<LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger>",
                    xml(&reg.context)
                )
            } else {
                String::new()
            };
            let arguments = format!(
                "-NoProfile -NonInteractive -WindowStyle Hidden -EncodedCommand {}",
                encoded(&windows_launcher(reg))
            );
            let document = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\"><RegistrationInfo><Description>SDU-InfoHelper electricity daemon</Description></RegistrationInfo><Triggers>{trigger}</Triggers><Principals><Principal id=\"Author\"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals><Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>true</AllowHardTerminate><StartWhenAvailable>true</StartWhenAvailable><RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable><AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled><Hidden>false</Hidden><RunOnlyIfIdle>false</RunOnlyIfIdle><WakeToRun>false</WakeToRun><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Priority>7</Priority><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings><Actions Context=\"Author\"><Exec><Command>powershell.exe</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions></Task>\n",
                xml(&reg.context),
                xml(&arguments),
                xml(&reg.working_directory)
            );
            let mut bytes = vec![0xff, 0xfe];
            bytes.extend(document.encode_utf16().flat_map(u16::to_le_bytes));
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn registration(platform: Platform, autostart: bool) -> Registration {
        Registration {
            platform,
            version: 1,
            name: "org.sdu-infohelper.test".into(),
            binary: "C:\\中文 folder\\prog'&$%.exe".into(),
            working_directory: "C:\\中文 folder".into(),
            arguments: vec![
                "daemon".into(),
                "run".into(),
                "--config".into(),
                "C:\\中文 folder\\config'&$%.yaml".into(),
                "--managed".into(),
            ],
            autostart,
            context: "S-1-5-21-123".into(),
            definition: PathBuf::from("test.xml"),
        }
    }
    #[test]
    fn linux_and_macos_quote_paths_without_shell_evaluation() {
        let linux = String::from_utf8(definition(&registration(Platform::Linux, false))).unwrap();
        assert!(linux.contains("$%%.yaml"));
        assert!(linux.contains("Restart=on-failure"));
        assert!(linux.contains("StartLimitBurst=3"));
        let mac = String::from_utf8(definition(&registration(Platform::Macos, true))).unwrap();
        assert!(mac.contains("config&apos;&amp;$%.yaml"));
        assert!(mac.contains("<key>SuccessfulExit</key><false/>"));
        assert!(mac.contains("<key>ExitTimeOut</key><integer>30</integer>"));
    }
    #[test]
    fn windows_task_is_user_scoped_indefinite_and_hidden_worker_has_safe_arguments() {
        for autostart in [false, true] {
            let reg = registration(Platform::Windows, autostart);
            let bytes = definition(&reg);
            assert_eq!(&bytes[..2], &[0xff, 0xfe]);
            let units: Vec<_> = bytes[2..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let xml = String::from_utf16(&units).unwrap();
            assert!(xml.contains("-WindowStyle Hidden"));
            assert_eq!(xml.contains("<LogonTrigger>"), autostart);
            for expected in [
                "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
                "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
                "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
                "<LogonType>InteractiveToken</LogonType>",
                "<WakeToRun>false</WakeToRun>",
                "<RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure>",
            ] {
                assert!(xml.contains(expected));
            }
            let script = windows_launcher(&reg);
            assert!(script.contains("CreateNoWindow=$true"));
            assert!(script.contains("prog''&$%.exe"));
            assert!(script.contains("--run-id"));
            assert!(script.contains("$p.WaitForExit()"));
        }
    }
    #[test]
    fn windows_quote_handles_backslashes_quotes_and_empty_arguments() {
        assert_eq!(windows_argument(""), "\"\"");
        assert_eq!(windows_argument("a b\\"), "\"a b\\\\\"");
        assert_eq!(windows_argument("a\\\"b"), "\"a\\\\\\\"b\"");
        assert_eq!(powershell_literal("x'$(command)"), "'x''$(command)'");
    }
}
