//! CLI-only argument schema and working-directory defaults.
use clap::{Args, Parser, Subcommand};
use rust_decimal::Decimal;
use sdu_infohelper::{SelectionLevel, auth, daemon};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "山大威海宿舍电费查询与本地监控")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(super) command: Command,
}

#[derive(Subcommand)]
pub(super) enum Command {
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
pub(super) enum DaemonCommand {
    /// 前台运行；系统后台任务也调用此入口
    Run(daemon::RunOptions),
    /// 注册当前用户后台任务；自启动需显式开启，不立即启动
    Install {
        #[arg(long)]
        autostart: bool,
    },
    /// 启动已安装的后台任务
    Start,
    /// 停止后重新启动并读取配置
    Restart {
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
        wait_seconds: u64,
    },
    /// 停止并移除服务注册，保留配置、历史和日志
    Uninstall {
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
        wait_seconds: u64,
    },
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
pub(super) enum AuthCommand {
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
pub(super) struct QueryArgs {
    #[arg(long, default_value_os_t = default_config())]
    pub(super) config: PathBuf,
    #[arg(long, default_value_os_t = default_history())]
    pub(super) history: PathBuf,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub(super) timeout: u64,
    #[arg(long)]
    pub(super) threshold: Option<Decimal>,
    #[arg(long)]
    pub(super) json: bool,
    #[command(flatten)]
    pub(super) location: LocationArgs,
}

#[derive(Args, Default)]
pub(super) struct LocationArgs {
    /// 校区参数值
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(super) campus: Option<String>,
    /// 楼栋参数值
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(super) building: Option<String>,
    /// 楼层参数值
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(super) floor: Option<String>,
    /// 房间参数值，来自 list rooms
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(super) room: Option<String>,
}

impl LocationArgs {
    pub(super) fn overrides(&self) -> BTreeMap<String, String> {
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
pub(super) struct ListArgs {
    #[arg(value_enum)]
    pub(super) level: SelectionLevel,
    #[arg(long, default_value_os_t = default_config())]
    pub(super) config: PathBuf,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub(super) timeout: u64,
    #[arg(long)]
    pub(super) json: bool,
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(super) campus: Option<String>,
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(super) building: Option<String>,
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub(super) floor: Option<String>,
}

#[derive(Args)]
pub(super) struct WatchArgs {
    #[command(flatten)]
    pub(super) query: QueryArgs,
    #[arg(long, default_value_t = 21600, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    pub(super) interval: u64,
    #[arg(long, default_value_t = 86400, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    pub(super) repeat_after: u64,
    /// 调用已有的 notify-send 发送 Linux 桌面通知
    #[arg(long)]
    pub(super) notify_desktop: bool,
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
