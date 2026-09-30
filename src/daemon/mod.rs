//! Portable worker and local lifecycle. Platform service adapters are a separate layer.
use crate::{
    Location, auth, history,
    monitor::{
        self, Comparison, Schedule,
        delivery::{self, ChannelState, DeliveryEngine},
    },
    notification::Alert,
    save_event,
    settings::{DaemonSettings, Settings},
};
use chrono::Utc;
use clap::Args;
use fs2::FileExt;
use md5::{Digest, Md5};
use rand::{RngCore, rngs::OsRng};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub mod platform;
mod signals;

#[derive(Args, Clone, Default)]
pub struct RunOptions {
    #[arg(long)]
    pub threshold: Option<Decimal>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    pub interval: Option<u64>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    pub repeat_after: Option<u64>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub timeout: Option<u64>,
    #[arg(long)]
    pub history: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub managed: bool,
    #[arg(long, hide = true)]
    pub run_id: Option<String>,
}

impl RunOptions {
    pub fn resolve(&self, settings: &Settings, config: &Path) -> Result<DaemonSettings, String> {
        let mut daemon = settings.daemon.clone();
        if let Some(value) = self.threshold {
            daemon.threshold_kwh = value.normalize();
        }
        if let Some(value) = self.interval {
            daemon.interval_seconds = value;
        }
        if let Some(value) = self.repeat_after {
            daemon.repeat_after_seconds = value;
        }
        if let Some(value) = self.timeout {
            daemon.query_timeout_seconds = value;
        }
        let history_override = self.history.clone().or_else(|| {
            (!self.managed)
                .then(|| std::env::var_os("SDU_INFOHELPER_HISTORY").map(PathBuf::from))
                .flatten()
        });
        daemon.history = match history_override {
            Some(path) if path.is_absolute() => path,
            Some(path) => std::env::current_dir()
                .map_err(|_| "无法读取工作目录。")?
                .join(path),
            None if daemon.history.is_absolute() => daemon.history,
            None => config
                .parent()
                .ok_or("配置目录不可用。")?
                .join(daemon.history),
        };
        daemon.validate().map_err(|e| e.to_string())?;
        Ok(daemon)
    }
}

#[derive(Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub instance: String,
    pub directory: PathBuf,
}

pub struct InstanceLock {
    pub run_id: String,
    _file: File,
}

fn random_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn private_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| "无法创建监控数据目录。".to_owned())?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path).map_err(|_| "无法创建监控数据目录。".to_owned())?;
    Ok(())
}

fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| "无法打开监控管理文件。".to_owned())
}

fn contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

impl Paths {
    pub fn new(config: &Path) -> Result<Self, String> {
        let absolute = if config.is_absolute() {
            config.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|_| "无法读取工作目录。")?
                .join(config)
        };
        // Allow management after an ordinary config file was deleted; its canonical parent still identifies the instance.
        let config = absolute
            .canonicalize()
            .or_else(|_| {
                let parent = absolute
                    .parent()
                    .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?
                    .canonicalize()?;
                Ok::<_, std::io::Error>(
                    parent.join(
                        absolute
                            .file_name()
                            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?,
                    ),
                )
            })
            .map_err(|_| "无法解析配置文件路径。")?;
        let encoded = config.to_str().ok_or("配置路径必须可以表示为 Unicode。")?;
        let instance = hex::encode(Md5::digest(encoded.as_bytes()));
        let directory = config
            .parent()
            .ok_or("配置目录不可用。")?
            .join(".local/electricity/daemon")
            .join(&instance);
        Ok(Self {
            config,
            instance,
            directory,
        })
    }
    pub fn ensure(&self) -> Result<(), String> {
        private_directory(&self.directory)
    }
    pub fn file(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }
    pub fn acquire(&self) -> Result<InstanceLock, String> {
        self.acquire_with_id(random_id())
    }
    fn acquire_with_id(&self, run_id: String) -> Result<InstanceLock, String> {
        self.ensure()?;
        let file = private_file(&self.file("run.lock"))?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            if contended(&error) {
                "该配置的监控已运行。"
            } else {
                "无法锁定监控实例。"
            }
        })?;
        // Windows byte-range locks prevent another handle from reading the locked
        // file. Publish identity separately while retaining the exclusive lifetime lock.
        auth::secure_write(&self.file("run.owner"), run_id.as_bytes())
            .map_err(|_| "无法发布监控实例标识。")?;
        Ok(InstanceLock {
            run_id,
            _file: file,
        })
    }
    pub fn running(&self) -> Result<bool, String> {
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.file("run.lock"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err("无法读取监控实例锁；运行状态未知。".into()),
        };
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(false),
            Err(error) if contended(&error) => Ok(true),
            Err(_) => Err("无法检查监控实例锁；运行状态未知。".into()),
        }
    }
    pub fn state(&self) -> Result<Option<RuntimeState>, String> {
        let file = match File::open(self.file("status.json")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("无法读取监控状态。".into()),
        };
        let mut bytes = Vec::new();
        file.take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|_| "无法读取监控状态。")?;
        if bytes.len() > 1_048_576 {
            return Err("监控状态文件过大。".into());
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "监控状态文件格式无效。".into())
    }
    fn save(&self, state: &RuntimeState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| "无法序列化监控状态。")?;
        auth::secure_write(&self.file("status.json"), &bytes)
            .map_err(|_| "无法保存监控状态。".into())
    }
    fn stop_requested(&self, run_id: &str) -> Result<bool, String> {
        let file = match File::open(self.file("stop.request")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err("无法读取停止请求。".into()),
        };
        let mut text = String::new();
        file.take(256)
            .read_to_string(&mut text)
            .map_err(|_| "无法读取停止请求。")?;
        Ok(text == run_id)
    }
    pub fn request_stop(&self) -> Result<bool, String> {
        for _ in 0..20 {
            if !self.running()? {
                return Ok(false);
            }
            let owner = self.owner()?;
            if let Some(state) = self.state()?
                && state.run_id == owner
            {
                auth::secure_write(&self.file("stop.request"), owner.as_bytes())
                    .map_err(|_| "无法保存停止请求。")?;
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err("监控正在启动，尚未提供当前实例状态，请稍后重试。".into())
    }
    fn owner(&self) -> Result<String, String> {
        let file = match File::open(self.file("run.owner")) {
            Ok(file) => file,
            // The worker may hold the lock briefly before publishing its identity.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
            Err(_) => return Err("无法读取实例标识。".into()),
        };
        let mut text = String::new();
        file.take(256)
            .read_to_string(&mut text)
            .map_err(|_| "无法读取实例标识。")?;
        Ok(text)
    }
    pub(crate) fn owner_matches(&self, run_id: &str) -> Result<bool, String> {
        Ok(self.owner()? == run_id)
    }
    pub fn wait_stopped(&self, seconds: u64) -> Result<bool, String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        while self.running()? {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            self.request_stop()?;
            thread::sleep(Duration::from_millis(100));
        }
        Ok(true)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct LastSuccess {
    pub checked_at: String,
    pub remaining_kwh: String,
    pub location: Option<Location>,
    pub supply_status: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct RuntimeState {
    pub run_id: String,
    pub pid: u32,
    #[serde(default)]
    pub executable: Option<PathBuf>,
    pub status: String,
    pub started_at: String,
    pub last_attempt_at: Option<String>,
    pub last_success: Option<LastSuccess>,
    pub next_check_at: Option<String>,
    pub last_error: Option<String>,
    pub channels: Vec<ChannelState>,
    #[serde(default)]
    pub recent_starts: Vec<i64>,
}

pub struct Log {
    paths: Paths,
    max_bytes: u64,
}

impl Log {
    pub fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
            max_bytes: 5 * 1024 * 1024,
        }
    }
    pub fn write(&self, message: &str) -> Result<(), String> {
        let line = format!(
            "[{}] {}\n",
            Utc::now().to_rfc3339(),
            message.replace(['\n', '\r'], " ")
        );
        let path = self.paths.file("daemon.log");
        if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > self.max_bytes {
            for index in (1..=3).rev() {
                let target = self.paths.file(&format!("daemon.log.{index}"));
                let source = if index == 1 {
                    path.clone()
                } else {
                    self.paths.file(&format!("daemon.log.{}", index - 1))
                };
                if target.exists() {
                    fs::remove_file(&target).map_err(|_| "无法轮转监控日志。")?;
                }
                if source.exists() {
                    fs::rename(source, target).map_err(|_| "无法轮转监控日志。")?;
                }
            }
        }
        let mut file = private_file(&path)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::End(0))
            .and_then(|_| file.write_all(line.as_bytes()))
            .map_err(|_| "无法写入监控日志。".into())
    }
}

fn timestamp(value: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(value, 0).map(|date| date.to_rfc3339())
}

fn secure_history(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "无法设置历史文件权限。")?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

pub fn run(config: &Path, options: &RunOptions) -> Result<(), String> {
    let paths = Paths::new(config)?;
    let run_id = options.run_id.clone().unwrap_or_else(random_id);
    if run_id.len() != 32 || !run_id.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("运行实例标识格式无效。".into());
    }
    let _lock = paths.acquire_with_id(run_id.clone())?;
    let _signals = signals::Guard::install()?;
    let log = Log::new(&paths);
    let previous = paths.state()?;
    let now = Utc::now().timestamp();
    let mut starts = previous
        .as_ref()
        .map(|state| state.recent_starts.clone())
        .unwrap_or_default();
    if options.managed {
        starts.retain(|time| now.saturating_sub(*time) <= 120);
    } else {
        starts.clear();
    }
    let mut state = RuntimeState {
        run_id,
        pid: std::process::id(),
        executable: std::env::current_exe().ok(),
        status: "starting".into(),
        started_at: Utc::now().to_rfc3339(),
        last_attempt_at: None,
        last_success: previous.and_then(|state| state.last_success),
        next_check_at: None,
        last_error: None,
        channels: Vec::new(),
        recent_starts: starts,
    };
    let result = (|| {
        if options.managed && state.recent_starts.len() >= 3 {
            return Err("监控短时间反复启动，已停止自动重启；请检查日志后重新 start。".into());
        }
        state.recent_starts.push(now);
        paths.save(&state)?;
        log.write("监控启动。")?;
        let settings = Settings::load(&paths.config).map_err(|e| e.to_string())?;
        let daemon = options.resolve(&settings, &paths.config)?;
        let channels = delivery::channels(&settings.notifications)?;
        if let Some(parent) = daemon.history.parent() {
            private_directory(parent)?;
        }
        let connection = history(&daemon.history).map_err(|_| "无法打开监控历史数据库。")?;
        secure_history(&daemon.history)?;
        let mut engine = DeliveryEngine::new(
            &connection,
            &paths.instance,
            channels,
            daemon.repeat_after_seconds,
            daemon.interval_seconds,
        )?;
        let mut schedule = Schedule::new(daemon.interval_seconds, Instant::now(), now);
        loop {
            if signals::requested() || paths.stop_requested(&state.run_id)? {
                break;
            }
            let now = Utc::now().timestamp();
            if schedule.due(Instant::now(), now) {
                schedule.started(Instant::now(), now);
                state.status = "checking".into();
                state.next_check_at = timestamp(schedule.next_wall());
                paths.save(&state)?;
                let sample = monitor::query_once(
                    &paths.config,
                    Duration::from_secs(daemon.query_timeout_seconds),
                    Some(daemon.threshold_kwh),
                    &BTreeMap::new(),
                    Comparison::StrictlyBelow,
                );
                save_event(&connection, &sample.event).map_err(|_| "无法保存监控查询记录。")?;
                state.last_attempt_at = Some(sample.event.checked_at.clone());
                state.last_error.clone_from(&sample.event.error);
                if sample.event.error.is_none() {
                    state.last_success = Some(LastSuccess {
                        checked_at: sample.event.checked_at.clone(),
                        remaining_kwh: sample.event.remaining_kwh.clone().unwrap_or_default(),
                        location: sample.event.location.clone(),
                        supply_status: sample.event.supply_status.clone(),
                    });
                }
                state.status = if sample.needs_login {
                    "needs_login"
                } else if sample.event.error.is_some() {
                    "query_failed"
                } else if sample.event.low_balance == Some(true) {
                    "low_balance"
                } else {
                    "healthy"
                }
                .into();
                let message = sample.event.error.clone().unwrap_or_else(|| {
                    format!(
                        "剩余电量 {} 度。",
                        sample.event.remaining_kwh.as_deref().unwrap_or("未知")
                    )
                });
                log.write(&message)?;
                if !options.managed {
                    println!("[{0}] {message}", sample.event.checked_at);
                }
                engine.observe(&sample.event, sample.needs_login, Utc::now().timestamp())?;
                state.channels = engine.states()?;
                paths.save(&state)?;
                if sample.fatal && !sample.needs_login {
                    return Err(message);
                }
                continue;
            }
            if let Some(report) = engine.dispatch_next(now)? {
                let message = if report.accepted {
                    format!("渠道 {}：推送服务已接受。", report.channel)
                } else {
                    format!(
                        "渠道 {}：{}",
                        report.channel,
                        report.error.as_deref().unwrap_or("发送失败。")
                    )
                };
                log.write(&message)?;
                if !options.managed {
                    println!("{message}");
                }
                state.channels = engine.states()?;
                paths.save(&state)?;
                continue;
            }
            thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    })();
    state.status = if result.is_ok() { "stopped" } else { "failed" }.into();
    state.next_check_at = None;
    if let Err(error) = &result {
        state.last_error = Some(error.clone());
    }
    paths.save(&state)?;
    log.write(if result.is_ok() {
        "监控已停止。"
    } else {
        state.last_error.as_deref().unwrap_or("监控失败。")
    })?;
    result
}

pub fn test_notification(config: &Path, selected: Option<&str>) -> Result<bool, String> {
    let settings = Settings::load(config).map_err(|e| e.to_string())?;
    let channels = delivery::channels(&settings.notifications)?;
    if selected.is_some_and(|id| !channels.iter().any(|channel| channel.id == id)) {
        return Err("找不到指定的启用通知渠道。".into());
    }
    let alert = Alert::test();
    let mut success = true;
    for channel in channels
        .into_iter()
        .filter(|channel| selected.is_none_or(|id| id == channel.id))
    {
        match channel.notifier.send(&alert) {
            Ok(_) => println!("渠道 {}：推送服务已接受。", channel.id),
            Err(error) => {
                eprintln!("渠道 {}：{error}", channel.id);
                success = false;
            }
        }
    }
    Ok(success)
}

#[derive(Serialize)]
pub struct LocalStatus {
    pub instance: String,
    pub running: bool,
    pub data_directory: PathBuf,
    pub runtime: Option<RuntimeState>,
    pub service: platform::ServiceStatus,
}

pub fn local_status(config: &Path) -> Result<LocalStatus, String> {
    let paths = Paths::new(config)?;
    let running = paths.running()?;
    let mut runtime = paths.state()?;
    if running {
        let owner = paths.owner()?;
        if runtime.as_ref().is_some_and(|state| owner != state.run_id) {
            runtime = None;
        }
    }
    if let (false, Some(state)) = (running, &mut runtime) {
        if !matches!(state.status.as_str(), "stopped" | "failed") {
            state.status = "terminated".into();
        }
        state.next_check_at = None;
    }
    Ok(LocalStatus {
        instance: paths.instance,
        running,
        data_directory: paths.directory,
        runtime,
        service: platform::status(config)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_waits_for_new_state_and_status_never_reports_stale_live_pid() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, "{}\n").unwrap();
        let paths = Paths::new(&config).unwrap();
        let lock = paths.acquire().unwrap();
        let mut state = RuntimeState {
            run_id: "old".into(),
            pid: 999999,
            executable: None,
            status: "healthy".into(),
            started_at: "old".into(),
            last_attempt_at: None,
            last_success: None,
            next_check_at: None,
            last_error: None,
            channels: Vec::new(),
            recent_starts: Vec::new(),
        };
        paths.save(&state).unwrap();
        let status = local_status(&config).unwrap();
        assert!(status.running);
        assert!(status.runtime.is_none());
        let stop_paths = paths.clone();
        let stopper = thread::spawn(move || stop_paths.request_stop());
        thread::sleep(Duration::from_millis(50));
        state.run_id.clone_from(&lock.run_id);
        paths.save(&state).unwrap();
        assert!(stopper.join().unwrap().unwrap());
        assert!(paths.stop_requested(&lock.run_id).unwrap());
    }

    #[test]
    fn canonical_config_identity_and_locks_are_independent_of_history() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("中文 path");
        fs::create_dir(&folder).unwrap();
        let config = folder.join("config.yaml");
        fs::write(&config, "{}\n").unwrap();
        let paths = Paths::new(&config).unwrap();
        assert!(!paths.running().unwrap());
        let lock = paths.acquire().unwrap();
        assert!(paths.running().unwrap());
        assert!(Paths::new(&config).unwrap().acquire().is_err());
        assert_eq!(
            Paths::new(&folder.join("./config.yaml")).unwrap().instance,
            paths.instance
        );
        drop(lock);
        assert!(!paths.running().unwrap());
        fs::remove_file(&config).unwrap();
        assert_eq!(Paths::new(&config).unwrap().instance, paths.instance);
    }

    #[test]
    fn yaml_paths_resolve_against_config_and_explicit_paths_are_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        let settings = Settings::default();
        let options = RunOptions {
            history: Some(dir.path().join("override.sqlite3")),
            threshold: Some(Decimal::ZERO),
            ..RunOptions::default()
        };
        let resolved = options.resolve(&settings, &config).unwrap();
        assert_eq!(resolved.history, dir.path().join("override.sqlite3"));
        assert_eq!(resolved.threshold_kwh, Decimal::ZERO);
        // The normal default is tested in CLI children with the history env var removed.
    }

    #[test]
    fn rotation_is_bounded_and_old_stop_requests_do_not_match_new_instances() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, "{}\n").unwrap();
        let paths = Paths::new(&config).unwrap();
        paths.ensure().unwrap();
        let log = Log {
            paths: paths.clone(),
            max_bytes: 100,
        };
        for _ in 0..10 {
            log.write(&"x".repeat(60)).unwrap();
        }
        for index in 1..=3 {
            assert!(paths.file(&format!("daemon.log.{index}")).exists());
        }
        assert!(!paths.file("daemon.log.4").exists());
        fs::write(paths.file("stop.request"), "old").unwrap();
        assert!(!paths.stop_requested("new").unwrap());
        assert!(paths.stop_requested("old").unwrap());
    }
}
