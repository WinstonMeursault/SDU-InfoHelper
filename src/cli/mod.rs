//! CLI adapter: argument parsing, command dispatch and presentation.
mod args;
mod authentication;
mod background;
mod history;
mod output;
mod queries;
mod watch;

pub(crate) use args::Cli;
use args::Command;
use sdu_infohelper::settings;

pub(crate) fn run(cli: Cli) -> Result<bool, String> {
    match cli.command {
        Command::Auth { command } => authentication::run(command),
        Command::Daemon { config, command } => background::run(&config, command),
        Command::Aircon {
            config,
            timeout,
            sms,
            trust_device,
            json,
        } => queries::aircon(&config, timeout, sms, trust_device, json),
        Command::Query(args) => queries::query(args),
        Command::List(args) => queries::list(args),
        Command::Watch(args) => watch::run(args),
        Command::History {
            history: path,
            limit,
        } => history::run(&path, limit),
        Command::CheckConfig { config } => {
            settings::Settings::load(&config).map_err(|e| e.to_string())?;
            println!("YAML 配置结构有效；账号、设备授信和服务可用性需登录验证。");
            Ok(true)
        }
    }
}
