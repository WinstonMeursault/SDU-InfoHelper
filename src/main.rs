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
use sdu_electricity::{
    Config, Event, Location, QueryError, SelectionLevel, alert_due, clear_alert, client, history,
    mark_alert, query, save_event, selection_options,
};

#[derive(Parser)]
#[command(version, about = "山大威海宿舍电费查询与本地监控")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 独立查询一次
    Query(QueryArgs),
    /// 定时查询、保存历史并提醒
    Watch(WatchArgs),
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".local/electricity/request.json")
}
fn default_history() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".local/electricity/history.sqlite3")
}

fn ensure_parent(path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|_| "无法创建本地数据目录。".to_owned())?;
    }
    Ok(())
}

fn secure_history(path: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "无法设置历史文件权限。".to_owned())?;
    }
    Ok(())
}

fn event(args: &QueryArgs) -> (Event, bool, bool) {
    let mut target = None;
    let result = Config::load(&args.config).and_then(|config| {
        let config = config.with_location_overrides(&args.location.overrides())?;
        let location = config.location()?;
        target = Some(location.clone());
        let connection = client(Duration::from_secs(args.timeout))?;
        let reading = query(&config, &connection)?;
        Ok(Event::success(reading, args.threshold, config.expiry_claim()).at_location(location))
    });
    match result {
        Ok(event) => (event, false, false),
        Err(error) => {
            let auth = matches!(error, QueryError::Authentication);
            let fatal = auth || matches!(error, QueryError::Config(_));
            let mut failure = Event::failure(&error);
            failure.location = target;
            (failure, fatal, auth)
        }
    }
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
        Command::List(args) => {
            let config = Config::load(&args.config).map_err(|error| error.to_string())?;
            let overrides = [
                ("campus", args.campus),
                ("building", args.building),
                ("floor", args.floor),
            ]
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
            .collect();
            let connection =
                client(Duration::from_secs(args.timeout)).map_err(|error| error.to_string())?;
            let options = selection_options(&config, &connection, args.level, &overrides)
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
                        "请通过 App 抓包更新凭据，然后重新启动监控。",
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
