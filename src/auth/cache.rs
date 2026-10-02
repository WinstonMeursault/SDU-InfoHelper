//! Atomic cache persistence and bounded, cancellable cross-process locking.
use super::{TokenCache, model::validate_token, secure_write};
use crate::{
    QueryError,
    control::{OperationControl, OperationError},
};
use fs2::FileExt;
use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub(super) fn load_cache(path: &Path) -> Result<Option<TokenCache>, QueryError> {
    match fs::read(path) {
        Ok(bytes) => {
            let cache: TokenCache = serde_json::from_slice(&bytes)
                .map_err(|_| QueryError::Config("本地认证缓存格式错误。"))?;
            validate_token(&cache.access_token)?;
            Ok(Some(cache))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(QueryError::Config("无法读取本地认证缓存。")),
    }
}
pub(super) fn save_cache(path: &Path, cache: &TokenCache) -> Result<(), QueryError> {
    secure_write(
        path,
        &serde_json::to_vec_pretty(cache)
            .map_err(|_| QueryError::Config("无法序列化认证缓存。"))?,
    )
}
pub(super) fn lock(
    path: &Path,
    control: &OperationControl<'_>,
) -> Result<fs::File, OperationError> {
    control.check()?;
    let path = path.with_extension("lock");
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|_| QueryError::Config("无法创建本地认证目录。"))?;
    // Open/create the same inode, rather than atomically replacing a contended lock file.
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(path)
        .map_err(|_| QueryError::Config("无法打开认证锁。"))?;
    let deadline = Instant::now() + control.timeout;
    loop {
        control.check()?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {}
            Err(_) => return Err(QueryError::Config("无法锁定认证缓存。").into()),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(QueryError::Response("等待本地认证缓存锁超时，请稍后重试。").into());
        }
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
}
