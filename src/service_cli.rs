use clap::{Args, Subcommand};
use sdu_infohelper::{
    auth,
    service::{Actor, PreferencePatch, Service, UserRequest},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    thread,
    time::Duration,
};

#[derive(Args)]
pub struct ServiceArgs {
    #[arg(long, global = true, default_value = ".local/service")]
    data_dir: PathBuf,
    #[arg(long, global=true, default_value_t=20, value_parser=clap::value_parser!(u64).range(1..=300))]
    timeout: u64,
    #[command(subcommand)]
    command: ServiceCommand,
}
#[derive(Args)]
struct IdentityArgs {
    /// 机器人 QQ 号（不同机器人之间也隔离）
    #[arg(long)]
    bot: String,
    /// 用户 QQ 号；仅供本地主机管理员选择，不接收聊天中的身份覆盖
    #[arg(long)]
    qq: String,
}
impl IdentityArgs {
    fn actor(&self) -> Result<Actor, String> {
        Actor::qq(&self.bot, &self.qq).map_err(|e| e.to_string())
    }
}
#[derive(Subcommand)]
enum ServiceCommand {
    /// 管理员从本地配置创建独立绑定；不会自动发送验证码
    Bind {
        #[command(flatten)]
        identity: IdentityArgs,
        #[arg(long)]
        config: PathBuf,
    },
    Status {
        #[command(flatten)]
        identity: IdentityArgs,
    },
    Query {
        #[command(flatten)]
        identity: IdentityArgs,
    },
    History {
        #[command(flatten)]
        identity: IdentityArgs,
        #[arg(long,default_value_t=20,value_parser=clap::value_parser!(u32).range(1..=1000))]
        limit: u32,
    },
    /// 修改该用户的提醒设置，持久保存
    Preferences {
        #[command(flatten)]
        identity: IdentityArgs,
        #[arg(long)]
        threshold: Option<String>,
        #[arg(long)]
        interval: Option<u64>,
        #[arg(long)]
        repeat_after: Option<u64>,
        #[arg(long, conflicts_with = "enable")]
        disable: bool,
        #[arg(long)]
        enable: bool,
    },
    /// 删除该用户的配置、令牌、历史和待发送提醒
    Unbind {
        #[command(flatten)]
        identity: IdentityArgs,
    },
    /// 在服务主机终端完成学校验证；成功后须 query 验证宿舍
    Login {
        #[command(flatten)]
        identity: IdentityArgs,
        #[arg(long)]
        sms: bool,
        #[arg(long)]
        trust_device: bool,
    },
    /// 管理员为该用户导入本地 OAuth 响应，不沿用其他用户的缓存路径
    ImportAuth {
        #[command(flatten)]
        identity: IdentityArgs,
        #[arg(long)]
        input: PathBuf,
        #[arg(long,value_enum,default_value_t=auth::OAuthProvider::Berserker)]
        provider: auth::OAuthProvider,
        #[arg(long)]
        client_auth_file: Option<PathBuf>,
    },
    /// 查看该用户待发送队列；不会确认送达
    Outbox {
        #[command(flatten)]
        identity: IdentityArgs,
    },
    /// 查询一轮已验证、开启监控且到期的用户
    Tick,
    /// 在前台运行多用户监控；QQ 发送由后续适配器接入
    Run {
        #[arg(long,default_value_t=30,value_parser=clap::value_parser!(u64).range(5..=3600))]
        poll_seconds: u64,
    },
    /// 本机可信适配器的 JSONL 请求通道；不会监听 HTTP/WS
    Stdio,
}
fn print(data: impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string(&data).map_err(|_| "服务输出序列化失败。")?
    );
    Ok(())
}
pub fn run(args: ServiceArgs) -> Result<bool, String> {
    let service = Service::open(&args.data_dir, Duration::from_secs(args.timeout))
        .map_err(|e| e.to_string())?;
    let result: Result<(), String> = (|| match args.command {
        ServiceCommand::Bind { identity, config } => print(
            service
                .provision(&identity.actor()?, &config)
                .map_err(|e| e.to_string())?,
        ),
        ServiceCommand::Status { identity } => print(
            service
                .status(&identity.actor()?)
                .map_err(|e| e.to_string())?,
        ),
        ServiceCommand::Query { identity } => {
            let event = service
                .query(&identity.actor()?)
                .map_err(|e| e.to_string())?;
            let success = event.error.is_none();
            print(event)?;
            if !success {
                return Err("该用户本次查询未获得有效电量。".into());
            }
            Ok(())
        }
        ServiceCommand::History { identity, limit } => print(
            service
                .history(&identity.actor()?, limit)
                .map_err(|e| e.to_string())?,
        ),
        ServiceCommand::Preferences {
            identity,
            threshold,
            interval,
            repeat_after,
            disable,
            enable,
        } => print(
            service
                .preferences(
                    &identity.actor()?,
                    PreferencePatch {
                        threshold_kwh: threshold,
                        interval_seconds: interval,
                        repeat_after_seconds: repeat_after,
                        enabled: if disable {
                            Some(false)
                        } else if enable {
                            Some(true)
                        } else {
                            None
                        },
                    },
                )
                .map_err(|e| e.to_string())?,
        ),
        ServiceCommand::Unbind { identity } => {
            service
                .unbind(&identity.actor()?)
                .map_err(|e| e.to_string())?;
            print(serde_json::json!({"unbound":true}))
        }
        ServiceCommand::Login {
            identity,
            sms,
            trust_device,
        } => print(
            service
                .login(&identity.actor()?, auth::LoginOptions { sms, trust_device })
                .map_err(|e| e.to_string())?,
        ),
        ServiceCommand::ImportAuth {
            identity,
            input,
            provider,
            client_auth_file,
        } => print(
            service
                .import_auth(
                    &identity.actor()?,
                    &input,
                    provider,
                    client_auth_file.as_deref(),
                )
                .map_err(|e| e.to_string())?,
        ),
        ServiceCommand::Outbox { identity } => print(
            service
                .outbox(&identity.actor()?)
                .map_err(|e| e.to_string())?,
        ),
        ServiceCommand::Tick => print(service.tick().map_err(|e| e.to_string())?),
        ServiceCommand::Run { poll_seconds } => loop {
            match service.tick() {
                Ok(report) => print(report)?,
                Err(_) => eprintln!("多用户监控本轮未完成，将在下一轮重试。"),
            }
            thread::sleep(Duration::from_secs(poll_seconds));
        },
        ServiceCommand::Stdio => stdio(&service),
    })();
    result.map(|()| true)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    request_id: String,
    bot_id: String,
    user_id: String,
    request: UserRequest,
}
fn stdio(service: &Service) -> Result<(), String> {
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    loop {
        let mut line = Vec::new();
        let length = input
            .by_ref()
            .take(65537)
            .read_until(b'\n', &mut line)
            .map_err(|_| "服务输入读取失败。")?;
        if length == 0 {
            return Ok(());
        }
        if length > 65536 {
            return Err("服务请求超出 64 KiB。".into());
        }
        let reply = match serde_json::from_slice::<Envelope>(&line) {
            Ok(envelope) if !envelope.request_id.is_empty() && envelope.request_id.len() <= 128 => {
                let result = Actor::qq(&envelope.bot_id, &envelope.user_id)
                    .and_then(|actor| service.handle(&actor, envelope.request));
                match result {
                    Ok(data) => {
                        serde_json::json!({"request_id":envelope.request_id,"ok":true,"data":data})
                    }
                    Err(error) => {
                        serde_json::json!({"request_id":envelope.request_id,"ok":false,"error_code":error.code(),"error":error.to_string()})
                    }
                }
            }
            _ => {
                serde_json::json!({"ok":false,"error_code":"invalid_request","error":"服务请求格式不匹配。"})
            }
        };
        writeln!(output, "{}", reply)
            .and_then(|()| output.flush())
            .map_err(|_| "服务输出写入失败。")?;
    }
}
