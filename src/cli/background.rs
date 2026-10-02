//! Native daemon management commands and status presentation.
use super::args::DaemonCommand;
use sdu_infohelper::daemon;
use std::path::Path;

pub(super) fn run(config: &Path, command: DaemonCommand) -> Result<bool, String> {
    match command {
        DaemonCommand::Install { autostart } => {
            let running = daemon::platform::install(config, autostart)?;
            println!(
                "后台服务已安装；登录自启动：{}。",
                if autostart { "开启" } else { "关闭" }
            );
            if running {
                println!("现有实例仍在运行；新注册参数在 restart 后应用。");
            } else {
                println!("执行 daemon start 启动监控。");
            }
            Ok(true)
        }
        DaemonCommand::Start => {
            daemon::platform::start(config)?;
            println!("监控已运行。");
            Ok(true)
        }
        DaemonCommand::Restart { wait_seconds } => {
            daemon::platform::restart(config, wait_seconds)?;
            println!("监控已重新启动。");
            Ok(true)
        }
        DaemonCommand::Uninstall { wait_seconds } => {
            daemon::platform::uninstall(config, wait_seconds)?;
            println!("后台服务已卸载，配置、历史和日志保留。");
            Ok(true)
        }
        DaemonCommand::Run(options) => {
            match daemon::run(config, &options) {
                Ok(()) => Ok(true),
                Err(error) if options.managed => {
                    // Known configuration/storage failures must not trigger manager restart loops.
                    eprintln!("{error}");
                    Ok(true)
                }
                Err(error) => Err(error),
            }
        }
        DaemonCommand::Stop { wait_seconds } => {
            let result = daemon::platform::stop(config, wait_seconds)?;
            println!(
                "{}",
                if result.forced {
                    "监控已由系统管理器终止。"
                } else if result.was_running {
                    "监控已正常停止。"
                } else {
                    "监控未运行。"
                }
            );
            Ok(true)
        }
        DaemonCommand::Status { json } => {
            let status = daemon::local_status(config)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&status).map_err(|_| "无法序列化监控状态。")?
                );
            } else {
                println!(
                    "监控：{}；数据目录：{}",
                    if status.running {
                        "运行中"
                    } else {
                        "未运行"
                    },
                    status.data_directory.display()
                );
                println!(
                    "后台注册：{}；自启动：{}；管理器状态：{}",
                    status.service.name.as_deref().unwrap_or("未安装"),
                    match status.service.autostart {
                        Some(true) => "开启",
                        Some(false) => "关闭",
                        None => "未知 / 未安装",
                    },
                    status
                        .service
                        .error
                        .as_deref()
                        .unwrap_or(if status.service.registered {
                            "可用"
                        } else {
                            "未注册"
                        })
                );
                if let Some(runtime) = &status.runtime {
                    println!(
                        "状态：{}；最近查询：{}；下次检查：{}",
                        runtime.status,
                        runtime.last_attempt_at.as_deref().unwrap_or("尚无"),
                        runtime.next_check_at.as_deref().unwrap_or("无")
                    );
                    if let Some(reading) = &runtime.last_success {
                        println!(
                            "最近成功读数：{} 度（{}）。",
                            reading.remaining_kwh, reading.checked_at
                        );
                    }
                    if let Some(error) = &runtime.last_error {
                        println!("最近错误：{error}");
                    }
                    for channel in &runtime.channels {
                        println!(
                            "渠道 {}：{}；最近错误：{}",
                            channel.id,
                            channel.status,
                            channel.last_error.as_deref().unwrap_or("无")
                        );
                    }
                }
            }
            Ok(true)
        }
        DaemonCommand::TestNotification { channel } => {
            daemon::test_notification(config, channel.as_deref())
        }
    }
}
