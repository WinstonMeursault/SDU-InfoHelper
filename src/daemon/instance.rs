//! Canonical instance identity, lifetime locks and local control files.
use super::RuntimeState;
use crate::local;
use fs2::FileExt;
use md5::{Digest, Md5};
use rand::{RngCore, rngs::OsRng};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

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

pub(super) fn random_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub(super) fn private_directory(path: &Path) -> Result<(), String> {
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

pub(super) fn private_file(path: &Path) -> Result<File, String> {
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
    pub(super) fn acquire_with_id(&self, run_id: String) -> Result<InstanceLock, String> {
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
        local::secure_write(&self.file("run.owner"), run_id.as_bytes())
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
    pub(super) fn save(&self, state: &RuntimeState) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|_| "无法序列化监控状态。")?;
        local::secure_write(&self.file("status.json"), &bytes)
            .map_err(|_| "无法保存监控状态。".into())
    }
    pub(super) fn stop_requested(&self, run_id: &str) -> Result<bool, String> {
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
                local::secure_write(&self.file("stop.request"), owner.as_bytes())
                    .map_err(|_| "无法保存停止请求。")?;
                return Ok(true);
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err("监控正在启动，尚未提供当前实例状态，请稍后重试。".into())
    }
    pub(super) fn owner(&self) -> Result<String, String> {
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
