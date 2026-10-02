//! Authentication CLI commands; authentication behavior belongs to the library.
use super::args::AuthCommand;
use sdu_infohelper::auth;
use std::time::Duration;

pub(super) fn run(command: AuthCommand) -> Result<bool, String> {
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
