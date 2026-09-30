//! Native user service registration. Worker credentials never enter service definitions.
mod backend;
mod command;
mod render;

use super::{Paths, RunOptions};
use crate::{auth, settings::Settings};
use command::{Runner, SystemRunner};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    fn current() -> Result<Self, String> {
        if cfg!(target_os = "linux") {
            Ok(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Ok(Self::Macos)
        } else if cfg!(windows) {
            Ok(Self::Windows)
        } else {
            Err("该系统没有后台管理适配；可使用 daemon run。".into())
        }
    }
    fn name(self, instance: &str) -> String {
        match self {
            Self::Linux => format!("sdu-infohelper-{instance}.service"),
            Self::Macos => format!("org.sdu-infohelper.{instance}"),
            Self::Windows => format!("SDU-InfoHelper-{instance}"),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u32,
    platform: Platform,
    name: String,
    binary: String,
    working_directory: String,
    arguments: Vec<String>,
    autostart: bool,
    context: String,
    definition: PathBuf,
}

#[derive(Serialize)]
pub struct ServiceStatus {
    pub registered: bool,
    pub available: Option<bool>,
    pub installed: Option<bool>,
    pub active: Option<bool>,
    pub autostart: Option<bool>,
    pub name: Option<String>,
    pub error: Option<String>,
}

pub struct StopResult {
    pub was_running: bool,
    pub forced: bool,
}

fn home() -> Result<PathBuf, String> {
    let path = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or("当前用户目录不可用。")?;
    if !path.is_absolute() {
        return Err("当前用户目录不是绝对路径。".into());
    }
    Ok(path)
}

fn definition_path(paths: &Paths, platform: Platform, autostart: bool, home: &Path) -> PathBuf {
    let name = platform.name(&paths.instance);
    match platform {
        Platform::Linux => home.join(".config/systemd/user").join(name),
        Platform::Macos if autostart => home
            .join("Library/LaunchAgents")
            .join(format!("{name}.plist")),
        Platform::Macos => paths.file(&format!("{name}.plist")),
        Platform::Windows => paths.file(&format!("{name}.xml")),
    }
}

fn base_arguments(paths: &Paths) -> Result<Vec<String>, String> {
    Ok(vec![
        "daemon".into(),
        "run".into(),
        "--config".into(),
        backend::path(&paths.config)?,
        "--managed".into(),
    ])
}

fn registration(
    paths: &Paths,
    autostart: bool,
    runner: &dyn Runner,
) -> Result<Registration, String> {
    let settings = Settings::load(&paths.config).map_err(|e| e.to_string())?;
    settings
        .notifications
        .require_enabled()
        .map_err(|e| e.to_string())?;
    let options = RunOptions::default();
    let resolved = options.resolve(&settings, &paths.config)?;
    let platform = Platform::current()?;
    let mut arguments = base_arguments(paths)?;
    if std::env::var_os("SDU_INFOHELPER_HISTORY").is_some() {
        arguments.extend(["--history".into(), backend::path(&resolved.history)?]);
    }
    let binary = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| "无法解析当前程序路径。")?;
    Ok(Registration {
        version: 1,
        platform,
        name: platform.name(&paths.instance),
        binary: backend::path(&binary)?,
        working_directory: backend::path(paths.config.parent().ok_or("配置目录不可用。")?)?,
        arguments,
        autostart,
        context: backend::context(platform, runner)?,
        definition: definition_path(paths, platform, autostart, &home()?),
    })
}

fn load(paths: &Paths, runner: &dyn Runner) -> Result<Option<Registration>, String> {
    let file = match File::open(paths.file("service.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("无法读取后台服务注册信息。".into()),
    };
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "无法读取后台服务注册信息。")?;
    if bytes.len() > 65536 {
        return Err("后台服务注册信息过大。".into());
    }
    let reg: Registration =
        serde_json::from_slice(&bytes).map_err(|_| "后台服务注册信息格式无效。")?;
    let platform = Platform::current()?;
    let expected = base_arguments(paths)?;
    let valid_arguments = reg.arguments.len() == 5 && reg.arguments == expected
        || reg.arguments.len() == 7
            && reg.arguments[..5] == expected
            && reg.arguments[5] == "--history"
            && Path::new(&reg.arguments[6]).is_absolute();
    if reg.version != 1
        || reg.platform != platform
        || reg.name != platform.name(&paths.instance)
        || reg.definition != definition_path(paths, platform, reg.autostart, &home()?)
        || reg.context != backend::context(platform, runner)?
        || !Path::new(&reg.binary).is_absolute()
        || reg.working_directory != backend::path(paths.config.parent().ok_or("配置目录不可用。")?)?
        || !valid_arguments
        || reg
            .arguments
            .iter()
            .chain([&reg.binary, &reg.working_directory])
            .any(|v| v.chars().any(char::is_control))
    {
        return Err("后台服务注册信息与当前实例不匹配。".into());
    }
    Ok(Some(reg))
}

fn manage_lock(paths: &Paths) -> Result<File, String> {
    paths.ensure()?;
    let file = super::private_file(&paths.file("manage.lock"))?;
    FileExt::try_lock_exclusive(&file).map_err(|_| "另一个后台管理操作正在执行。")?;
    Ok(file)
}

pub fn install(config: &Path, autostart: bool) -> Result<bool, String> {
    let paths = Paths::new(config)?;
    let _lock = manage_lock(&paths)?;
    let runner = SystemRunner;
    let previous = load(&paths, &runner)?;
    let reg = registration(&paths, autostart, &runner)?;
    backend::available(&reg, &runner)?;
    let old_bytes = fs::read(&reg.definition).ok();
    auth::secure_write(&reg.definition, &render::definition(&reg))
        .map_err(|_| "无法保存后台服务定义。")?;
    if let Err(error) = backend::install(&reg, &runner) {
        if let Some(bytes) = old_bytes {
            let _ = auth::secure_write(&reg.definition, &bytes);
        } else {
            let _ = backend::remove(&reg.definition);
        }
        return Err(error);
    }
    let bytes = serde_json::to_vec_pretty(&reg).map_err(|_| "无法序列化服务注册信息。")?;
    auth::secure_write(&paths.file("service.json"), &bytes)
        .map_err(|_| "服务已注册，但本地注册信息保存失败；请重新 install。")?;
    if let Some(previous) = previous
        && previous.definition != reg.definition
    {
        backend::remove(&previous.definition)?;
    }
    paths.running()
}

fn start_inner(paths: &Paths, reg: &Registration, runner: &dyn Runner) -> Result<(), String> {
    backend::available(reg, runner)?;
    if paths.running()? {
        return Ok(());
    }
    let settings = Settings::load(&paths.config).map_err(|e| e.to_string())?;
    settings
        .notifications
        .require_enabled()
        .map_err(|e| e.to_string())?;
    let previous = paths.state()?.map(|state| state.run_id);
    if let Some(mut state) = paths.state()? {
        state.recent_starts.clear();
        paths.save(&state)?;
    }
    backend::start(reg, runner)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(state) = paths.state()?
            && previous.as_deref() != Some(&state.run_id)
        {
            if state.status == "failed" {
                return Err(state
                    .last_error
                    .unwrap_or_else(|| "后台工作进程启动失败。".into()));
            }
            if paths.running()? && paths.owner_matches(&state.run_id)? {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err("服务启动后未发现当前监控实例；请检查 daemon status 和日志。".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

pub fn start(config: &Path) -> Result<(), String> {
    let paths = Paths::new(config)?;
    let _lock = manage_lock(&paths)?;
    let runner = SystemRunner;
    let reg = load(&paths, &runner)?.ok_or("尚未安装后台服务，请先执行 daemon install。")?;
    start_inner(&paths, &reg, &runner)
}

fn stop_inner(
    paths: &Paths,
    reg: Option<&Registration>,
    runner: &dyn Runner,
    wait_seconds: u64,
) -> Result<StopResult, String> {
    let was_running = paths.running()?;
    if was_running {
        paths.request_stop()?;
    }
    let graceful = !was_running || paths.wait_stopped(wait_seconds)?;
    if let Some(reg) = reg {
        backend::available(reg, runner)?;
        backend::stop(reg, paths, runner)?; // Also cancel an outstanding manager restart/start.
        if !paths.wait_stopped(5)? {
            return Err("系统管理器停止后监控实例仍在运行。".into());
        }
    } else if !graceful {
        return Err("监控未在等待时间内停止；当前网络操作仍受配置超时限制。".into());
    }
    let forced = was_running
        && (!graceful
            || paths
                .state()?
                .is_some_and(|state| state.status != "stopped"));
    Ok(StopResult {
        was_running,
        forced,
    })
}

pub fn stop(config: &Path, wait_seconds: u64) -> Result<StopResult, String> {
    let paths = Paths::new(config)?;
    let _lock = manage_lock(&paths)?;
    let runner = SystemRunner;
    let reg = load(&paths, &runner)?;
    stop_inner(&paths, reg.as_ref(), &runner, wait_seconds)
}

pub fn restart(config: &Path, wait_seconds: u64) -> Result<(), String> {
    let paths = Paths::new(config)?;
    let _lock = manage_lock(&paths)?;
    let runner = SystemRunner;
    let reg = load(&paths, &runner)?.ok_or("尚未安装后台服务，请先执行 daemon install。")?;
    stop_inner(&paths, Some(&reg), &runner, wait_seconds)?;
    start_inner(&paths, &reg, &runner)
}

pub fn uninstall(config: &Path, wait_seconds: u64) -> Result<(), String> {
    let paths = Paths::new(config)?;
    let _lock = manage_lock(&paths)?;
    let runner = SystemRunner;
    let Some(reg) = load(&paths, &runner)? else {
        return Ok(());
    };
    stop_inner(&paths, Some(&reg), &runner, wait_seconds)?;
    backend::uninstall(&reg, &runner)?;
    backend::remove(&paths.file("service.json"))
}

pub fn status(config: &Path) -> Result<ServiceStatus, String> {
    let paths = Paths::new(config)?;
    let runner = SystemRunner;
    let Some(reg) = load(&paths, &runner)? else {
        return Ok(ServiceStatus {
            registered: false,
            available: None,
            installed: None,
            active: None,
            autostart: None,
            name: None,
            error: None,
        });
    };
    match backend::available(&reg, &runner).and_then(|_| backend::inspect(&reg, &runner)) {
        Ok(status) => Ok(status),
        Err(error) => Ok(ServiceStatus {
            registered: true,
            available: Some(false),
            installed: None,
            active: None,
            autostart: None,
            name: Some(reg.name),
            error: Some(error),
        }),
    }
}
