//! History file setup and presentation; SQLite queries live in the store.
use sdu_infohelper::storage::HistoryStore;
use std::{fs, path::Path};

pub(super) fn open_for_write(path: &Path) -> Result<HistoryStore, String> {
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|_| "无法创建本地数据目录。".to_owned())?;
    }
    let store = HistoryStore::open(path).map_err(|_| "无法写入历史文件。")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "无法设置历史文件权限。")?;
    }
    Ok(store)
}

pub(super) fn run(path: &Path, limit: u32) -> Result<bool, String> {
    if !path.exists() {
        return Err("尚无历史记录，请先查询一次。".into());
    }
    let store = HistoryStore::open(path).map_err(|_| "无法读取历史文件。")?;
    let records = store.recent(limit).map_err(|_| "无法读取历史记录。")?;
    for record in records {
        println!(
            "{}\t{}\t{}\t{}",
            record.checked_at,
            record
                .location
                .map(|location| location.label())
                .unwrap_or_else(|| "宿舍未记录".into()),
            record
                .remaining_kwh
                .map(|value| format!("{value} 度"))
                .unwrap_or_else(|| "查询失败".into()),
            record.error.or(record.supply_status).unwrap_or_default()
        );
    }
    Ok(true)
}
