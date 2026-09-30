use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::{Command as ProcessCommand, ExitCode},
    thread,
    time::Duration,
};

use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use rust_decimal::Decimal;
use sdu_infohelper::{
    Event, Location, SelectionLevel, aircon, alert_due, auth, clear_alert, daemon, history,
    mark_alert, monitor, save_event, selection_options, settings,
    with_dorm_directory_auth_overrides,
};

#[derive(Parser)]
#[command(version, about = "山大威海宿舍电费查询与本地监控")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 统一身份认证、令牌导入和登录状态
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// 查询母仓库的空调电量（独立于宿舍普通用电）
    Aircon {
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
        #[arg(long)]
        sms: bool,
        #[arg(long)]
        trust_device: bool,
        #[arg(long)]
        json: bool,
    },
    /// 检查统一 YAML 配置结构，不访问网络
    CheckConfig {
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
    },
    /// 独立查询一次
    Query(QueryArgs),
    /// 定时查询、保存历史并提醒
    Watch(WatchArgs),
    /// 跨平台常驻监控及通知渠道测试
    Daemon {
        #[arg(long, global = true, default_value_os_t = default_config())]
        config: PathBuf,
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// 列出校区、楼栋、楼层或房间，使用返回的参数值查询
    List(ListArgs),
    /// 查看近期查询记录
    History {
        #[arg(long, default_value_os_t = default_history())]
        history: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// 前台运行；系统后台任务也调用此入口
    Run(daemon::RunOptions),
    /// 请求当前实例优雅停止
    Stop {
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
        wait_seconds: u64,
    },
    /// 查看进程及最近查询和推送状态
    Status {
        #[arg(long)]
        json: bool,
    },
    /// 显式发送测试消息，不影响预警冷却
    TestNotification {
        #[arg(long)]
        channel: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// 立即续期缓存的令牌，用于验证刷新或 CAS 回退
    Renew {
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    /// 登录宿舍平台；短信只在显式指定时发送
    Login {
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
        #[arg(long)]
        sms: bool,
        #[arg(long)]
        trust_device: bool,
    },
    /// 只检查本地令牌缓存，不输出令牌
    Status {
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
    },
    /// 匿名检查 CAS 登录页与表单，无需账号
    Probe {
        #[arg(long)]
        aircon: bool,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    /// 从本地 OAuth 登录响应 JSON 或旧 request.json 导入凭据
    Import {
        #[arg(long, default_value_os_t = default_config())]
        config: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long, value_enum, default_value_t = auth::OAuthProvider::Berserker)]
        provider: auth::OAuthProvider,
        /// 本地文件保存的一行 Basic 认证头；不在命令行填写秘密
        #[arg(long)]
        client_auth_file: Option<PathBuf>,
    },
}

#[derive(Args)]
struct QueryArgs {
    #[arg(long, default_value_os_t = default_config())]
    config: PathBuf,
    #[arg(long, default_value_os_t = default_history())]
    history: PathBuf,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout: u64,
    #[arg(long)]
    threshold: Option<Decimal>,
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    location: LocationArgs,
}

#[derive(Args, Default)]
struct LocationArgs {
    /// 校区参数值
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    campus: Option<String>,
    /// 楼栋参数值
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    building: Option<String>,
    /// 楼层参数值
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    floor: Option<String>,
    /// 房间参数值，来自 list rooms
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    room: Option<String>,
}

impl LocationArgs {
    fn overrides(&self) -> BTreeMap<String, String> {
        [
            ("campus", &self.campus),
            ("building", &self.building),
            ("floor", &self.floor),
            ("room", &self.room),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key.to_owned(), value.clone())))
        .collect()
    }
}

#[derive(Args)]
struct ListArgs {
    #[arg(value_enum)]
    level: SelectionLevel,
    #[arg(long, default_value_os_t = default_config())]
    config: PathBuf,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout: u64,
    #[arg(long)]
    json: bool,
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    campus: Option<String>,
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    building: Option<String>,
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    floor: Option<String>,
}

#[derive(Args)]
struct WatchArgs {
    #[command(flatten)]
    query: QueryArgs,
    #[arg(long, default_value_t = 21600, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    interval: u64,
    #[arg(long, default_value_t = 86400, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    repeat_after: u64,
    /// 调用已有的 notify-send 发送 Linux 桌面通知
    #[arg(long)]
    notify_desktop: bool,
}

fn default_config() -> PathBuf {
    std::env::var_os("SDU_INFOHELPER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.yaml"))
}
fn default_history() -> PathBuf {
    std::env::var_os("SDU_INFOHELPER_HISTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/electricity/history.sqlite3"))
}

fn ensure_parent(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|_| "无法创建本地数据目录。".to_owned())?;
    }
    Ok(())
}

#[cfg(unix)]
fn secure_history(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| "无法设置历史文件权限。".to_owned())
}

#[cfg(not(unix))]
fn secure_history(_path: &std::path::Path) -> Result<(), String> {
    Ok(())
}

fn event(args: &QueryArgs) -> (Event, bool, bool) {
    let sample = monitor::query_once(
        &args.config,
        Duration::from_secs(args.timeout),
        args.threshold,
        &args.location.overrides(),
        monitor::Comparison::Inclusive,
    );
    (sample.event, sample.fatal, sample.needs_login)
}

fn print_event(event: &Event, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(event).expect("event serialization")
        );
    } else if let Some(error) = &event.error {
        eprintln!("[{}] 查询失败：{error}", event.checked_at);
    } else {
        if let Some(location) = &event.location {
            println!("宿舍：{}", location.label());
        }
        println!(
            "[{}] 剩余电量：{} 度",
            event.checked_at,
            event.remaining_kwh.as_deref().unwrap_or("未知")
        );
        if let Some(supply) = &event.supply_status {
            println!("供电状态（接口原文）：{supply}");
        }
    }
}

fn alert(title: &str, message: &str, desktop: bool) -> bool {
    eprintln!("提醒：{title} — {message}");
    if !desktop {
        return true;
    }
    match ProcessCommand::new("notify-send")
        .args([
            "--app-name=SDU-InfoHelper",
            "--urgency=critical",
            title,
            message,
        ])
        .status()
    {
        Ok(status) if status.success() => true,
        _ => {
            eprintln!("桌面通知未发送成功，提醒已保留在终端日志。");
            false
        }
    }
}

fn run(cli: Cli) -> Result<bool, String> {
    match cli.command {
        Command::Daemon { config, command } => match command {
            DaemonCommand::Run(options) => {
                match daemon::run(&config, &options) {
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
                let paths = daemon::Paths::new(&config)?;
                if !paths.request_stop()? {
                    println!("监控未运行。");
                } else if paths.wait_stopped(wait_seconds)? {
                    println!("监控已正常停止。");
                } else {
                    return Err("监控未在等待时间内停止；当前网络操作仍受配置超时限制。".into());
                }
                Ok(true)
            }
            DaemonCommand::Status { json } => {
                let status = daemon::local_status(&config)?;
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
                daemon::test_notification(&config, channel.as_deref())
            }
        },
        Command::CheckConfig { config } => {
            settings::Settings::load(&config).map_err(|e| e.to_string())?;
            println!("YAML 配置结构有效；账号、设备授信和服务可用性需登录验证。");
            Ok(true)
        }
        Command::Auth { command } => {
            let status = match command {
                AuthCommand::Renew { config, timeout } => {
                    auth::renew(&config, Duration::from_secs(timeout))
                }
                AuthCommand::Login {
                    config,
                    timeout,
                    sms,
                    trust_device,
                } => auth::login(
                    &config,
                    Duration::from_secs(timeout),
                    auth::LoginOptions { sms, trust_device },
                ),
                AuthCommand::Status { config } => auth::status(&config),
                AuthCommand::Import {
                    config,
                    input,
                    provider,
                    client_auth_file,
                } => auth::import(&config, &input, provider, client_auth_file.as_deref()),
                AuthCommand::Probe { aircon, timeout } => {
                    auth::probe(aircon, Duration::from_secs(timeout)).map_err(|e| e.to_string())?;
                    println!("学校 CAS 登录页可达，动态表单有效。账号登录尚未验证。");
                    return Ok(true);
                }
            }
            .map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string(&status).map_err(|_| "认证状态序列化失败。")?
            );
            Ok(true)
        }
        Command::Aircon {
            config,
            timeout,
            sms,
            trust_device,
            json,
        } => {
            let reading = aircon::query_config(
                &config,
                Duration::from_secs(timeout),
                auth::LoginOptions { sms, trust_device },
            )
            .map_err(|e| e.to_string())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&reading).map_err(|_| "空调读数序列化失败。")?
                );
            } else {
                println!(
                    "{} 公寓 / {} 层 / {} 房间：空调剩余 {} 度",
                    reading.building, reading.floor, reading.room, reading.remaining_kwh
                );
            }
            Ok(true)
        }
        Command::List(args) => {
            let overrides = [
                ("campus", args.campus),
                ("building", args.building),
                ("floor", args.floor),
            ]
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
            .collect();
            let options = with_dorm_directory_auth_overrides(
                &args.config,
                Duration::from_secs(args.timeout),
                args.level,
                &overrides,
                |config, connection| {
                    selection_options(config, connection, args.level, &BTreeMap::new())
                },
            )
            .map_err(|error| error.to_string())?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string(&options).map_err(|_| "目录序列化失败。")?
                );
            } else {
                println!("名称\t参数值");
                for option in options {
                    println!("{}\t{}", option.name, option.value);
                }
            }
            Ok(true)
        }
        Command::History {
            history: path,
            limit,
        } => {
            if !path.exists() {
                return Err("尚无历史记录，请先查询一次。".into());
            }
            let connection = history(&path).map_err(|_| "无法读取历史文件。")?;
            let mut statement = connection.prepare(
                "SELECT checked_at, remaining_kwh, supply_status, error, campus, building, floor, room FROM readings ORDER BY id DESC LIMIT ?1"
            ).map_err(|_| "无法读取历史记录。")?;
            let rows = statement
                .query_map([limit], |row| {
                    let location = match (
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ) {
                        (Some(campus), Some(building), Some(floor), Some(room)) => Some(Location {
                            campus,
                            building,
                            floor,
                            room,
                        }),
                        _ => None,
                    };
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        location,
                    ))
                })
                .map_err(|_| "无法读取历史记录。")?;
            for row in rows {
                let (at, energy, supply, error, location) =
                    row.map_err(|_| "无法读取历史记录。")?;
                println!(
                    "{at}\t{}\t{}\t{}",
                    location
                        .map(|location| location.label())
                        .unwrap_or_else(|| "宿舍未记录".into()),
                    energy
                        .map(|value| format!("{value} 度"))
                        .unwrap_or_else(|| "查询失败".into()),
                    error.or(supply).unwrap_or_default()
                );
            }
            Ok(true)
        }
        Command::Query(args) => {
            ensure_parent(&args.history)?;
            let connection = history(&args.history).map_err(|_| "无法写入历史文件。")?;
            secure_history(&args.history)?;
            let (event, _, _) = event(&args);
            save_event(&connection, &event).map_err(|_| "无法写入查询记录。")?;
            print_event(&event, args.json);
            if event.low_balance == Some(true) {
                alert(
                    "宿舍电量不足",
                    &format!(
                        "剩余 {} 度，阈值 {} 度。",
                        event.remaining_kwh.as_deref().unwrap_or("未知"),
                        event.threshold_kwh.as_deref().unwrap_or("未知")
                    ),
                    false,
                );
            }
            Ok(event.error.is_none())
        }
        Command::Watch(mut args) => {
            args.query.threshold.get_or_insert(Decimal::from(10));
            ensure_parent(&args.query.history)?;
            let connection = history(&args.query.history).map_err(|_| "无法写入历史文件。")?;
            secure_history(&args.query.history)?;
            loop {
                let (event, fatal, auth) = event(&args.query);
                save_event(&connection, &event).map_err(|_| "无法写入查询记录。")?;
                print_event(&event, args.query.json);
                let now = Utc::now().timestamp();
                let topic = format!(
                    "low:{}:{}",
                    event
                        .location
                        .as_ref()
                        .map(|location| serde_json::to_string(location)
                            .expect("location serialization"))
                        .unwrap_or_default(),
                    event.threshold_kwh.as_deref().unwrap_or("")
                );
                if event.low_balance == Some(true) {
                    if alert_due(&connection, &topic, now, args.repeat_after)
                        .map_err(|_| "无法读取提醒记录。")?
                        && alert(
                            "宿舍电量不足",
                            &format!(
                                "剩余 {} 度，阈值 {} 度。",
                                event.remaining_kwh.as_deref().unwrap_or("未知"),
                                event.threshold_kwh.as_deref().unwrap_or("未知")
                            ),
                            args.notify_desktop,
                        )
                    {
                        mark_alert(&connection, &topic, now).map_err(|_| "无法保存提醒记录。")?;
                    }
                } else if event.low_balance == Some(false) {
                    clear_alert(&connection, &topic).map_err(|_| "无法更新提醒记录。")?;
                }
                if auth {
                    alert(
                        "电费监控登录已失效",
                        "请在本机运行 auth login --trust-device 完成登录，然后重新启动监控。",
                        args.notify_desktop,
                    );
                }
                if fatal {
                    return Ok(false);
                }
                thread::sleep(Duration::from_secs(args.interval));
            }
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdu_infohelper::QueryError;

    #[test]
    fn second_factor_requirement_stops_watch_and_requests_attention() {
        assert_eq!(
            monitor::failure_flags(&QueryError::AuthFlow("需要二次验证")),
            (true, true)
        );
        assert_eq!(monitor::failure_flags(&QueryError::Network), (false, false));
        for error in [
            QueryError::Http(408),
            QueryError::Http(429),
            QueryError::Response("刷新响应暂不可用"),
        ] {
            assert_eq!(monitor::failure_flags(&error), (false, false));
        }
    }
}
