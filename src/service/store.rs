use super::{AccountStatus, Preferences, ServiceError};
use fs2::FileExt;
use rand::{RngCore, rngs::OsRng};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) fn fresh_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
pub(super) fn existing_directory(path: &Path) -> Result<(), ServiceError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ServiceError::Storage)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ServiceError::Storage);
    }
    Ok(())
}
pub(super) fn directory(path: &Path) -> Result<(), ServiceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| ServiceError::Storage)?;
        existing_directory(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| ServiceError::Storage)?;
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path).map_err(|_| ServiceError::Storage)?;
        existing_directory(path)?;
    }
    Ok(())
}
fn file(path: &Path) -> Result<fs::File, ServiceError> {
    if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err(ServiceError::Storage);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path).map_err(|_| ServiceError::Storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| ServiceError::Storage)?;
    }
    Ok(file)
}
pub(super) fn lock(path: &Path) -> Result<fs::File, ServiceError> {
    let file = file(path)?;
    file.try_lock_exclusive().map_err(|_| ServiceError::Busy)?;
    Ok(file)
}
pub(super) fn database(path: &Path) -> Result<Connection, ServiceError> {
    file(path)?;
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}
pub(super) struct Guard {
    pub dir: PathBuf,
    pub _lock: fs::File,
}
#[derive(Deserialize, Serialize)]
pub(super) struct Profile {
    pub state: String,
    pub preferences: Preferences,
    pub last_check_at: Option<i64>,
    pub last_manual_at: Option<i64>,
    pub next_check_at: i64,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            state: "pending".into(),
            preferences: Preferences::default(),
            last_check_at: None,
            last_manual_at: None,
            next_check_at: 0,
        }
    }
}
impl Profile {
    pub fn status(&self) -> AccountStatus {
        AccountStatus {
            state: self.state.clone(),
            preferences: self.preferences.clone(),
            last_check_at: self.last_check_at,
            next_check_at: self.next_check_at,
        }
    }
}
impl Guard {
    pub fn database(&self) -> Result<Connection, ServiceError> {
        let path = self.dir.join("history.sqlite3");
        file(&path)?;
        let connection = crate::history(&path)?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS profile (id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS outbox (id TEXT PRIMARY KEY,topic TEXT NOT NULL,message TEXT NOT NULL,
            created_at INTEGER NOT NULL,state TEXT NOT NULL,lease_token TEXT,lease_until INTEGER,
            attempts INTEGER NOT NULL DEFAULT 0,retry_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS outbox_pending ON outbox(state,retry_at);")?;
        Ok(connection)
    }
    pub fn profile(&self, connection: &Connection) -> Result<Profile, ServiceError> {
        let data: String =
            connection.query_row("SELECT data FROM profile WHERE id=1", [], |row| row.get(0))?;
        let profile: Profile = serde_json::from_str(&data).map_err(|_| ServiceError::Storage)?;
        profile.preferences.validate()?;
        Ok(profile)
    }
    pub fn save_profile(
        &self,
        connection: &Connection,
        profile: &Profile,
    ) -> Result<(), ServiceError> {
        connection.execute("INSERT INTO profile (id,data) VALUES (1,?1) ON CONFLICT(id) DO UPDATE SET data=excluded.data", [serde_json::to_string(profile).map_err(|_| ServiceError::Storage)?])?;
        Ok(())
    }
}
pub(super) fn cancel(connection: &Connection, topic: &str) -> Result<(), ServiceError> {
    connection.execute("UPDATE outbox SET state='cancelled',lease_token=NULL WHERE topic=?1 AND state IN ('pending','leased')", [topic])?;
    Ok(())
}
pub(super) fn enqueue(
    connection: &Connection,
    topic: &str,
    message: &str,
    now: i64,
    repeat: u64,
) -> Result<(), ServiceError> {
    connection.execute("UPDATE outbox SET state='cancelled',lease_token=NULL WHERE topic=?1 AND state IN ('pending','leased') AND created_at<?2", params![topic,now.saturating_sub(86400)])?;
    let pending: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbox WHERE topic=?1 AND state IN ('pending','leased'))",
        [topic],
        |row| row.get(0),
    )?;
    if !pending && crate::alert_due(connection, topic, now, repeat)? {
        connection.execute("INSERT INTO outbox (id,topic,message,created_at,state,retry_at) VALUES (?1,?2,?3,?4,'pending',?4)", params![fresh_id(),topic,message,now])?;
    }
    Ok(())
}
