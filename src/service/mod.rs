//! Multi-user core. Actors are supplied by a trusted adapter, never by command text.
//! No QQ network connection is opened by this module.
pub mod napcat;
mod store;

use crate::{Event, Location, QueryError, Reading, auth, settings::Settings};
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use store::{Guard, Profile, fresh_id};

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("该 QQ 用户尚未绑定。")]
    NotBound,
    #[error("该 QQ 用户已有绑定，请先解绑。")]
    AlreadyBound,
    #[error("该用户正在执行其他操作，请稍后重试。")]
    Busy,
    #[error("查询过于频繁，请间隔至少 10 秒。")]
    RateLimited,
    #[error("无法读写服务数据。")]
    Storage,
    #[error("提醒租约已失效或不属于该用户。")]
    InvalidLease,
    #[error(transparent)]
    School(#[from] QueryError),
}
impl From<rusqlite::Error> for ServiceError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}
impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_request",
            Self::NotBound => "not_bound",
            Self::AlreadyBound => "already_bound",
            Self::Busy => "busy",
            Self::RateLimited => "rate_limited",
            Self::Storage => "storage_error",
            Self::InvalidLease => "invalid_lease",
            Self::School(_) => "school_error",
        }
    }
}

/// This identity must come from authenticated bot events or the local administrator.
/// There is deliberately no Deserialize implementation for a user-supplied body.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Actor {
    bot_id: String,
    user_id: String,
}
impl Actor {
    pub fn qq(bot_id: &str, user_id: &str) -> Result<Self, ServiceError> {
        fn valid(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 19
                && !value.starts_with('0')
                && value.bytes().all(|c| c.is_ascii_digit())
                && value.parse::<u64>().is_ok_and(|n| n <= i64::MAX as u64)
        }
        if !valid(bot_id) || !valid(user_id) {
            return Err(ServiceError::Invalid("QQ 号必须是规范的正整数。"));
        }
        if bot_id == user_id {
            return Err(ServiceError::Invalid("不能为机器人自身绑定。"));
        }
        Ok(Self {
            bot_id: bot_id.into(),
            user_id: user_id.into(),
        })
    }
    pub fn bot_id(&self) -> &str {
        &self.bot_id
    }
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub enabled: bool,
    pub threshold_kwh: String,
    pub interval_seconds: u64,
    pub repeat_after_seconds: u64,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_kwh: "10".into(),
            interval_seconds: 21600,
            repeat_after_seconds: 86400,
        }
    }
}
impl Preferences {
    fn threshold(&self) -> Result<Decimal, ServiceError> {
        self.threshold_kwh
            .parse::<Decimal>()
            .ok()
            .filter(|n| *n >= Decimal::ZERO && *n <= Decimal::from(10000))
            .ok_or(ServiceError::Invalid(
                "提醒阈值必须为 0 到 10000 度之间的数字。",
            ))
    }
    fn validate(&self) -> Result<(), ServiceError> {
        self.threshold()?;
        if !(300..=604800).contains(&self.interval_seconds) {
            return Err(ServiceError::Invalid("查询间隔须为 300 到 604800 秒。"));
        }
        if !(3600..=604800).contains(&self.repeat_after_seconds) {
            return Err(ServiceError::Invalid(
                "重复提醒间隔须为 3600 到 604800 秒。",
            ));
        }
        Ok(())
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencePatch {
    pub enabled: Option<bool>,
    pub threshold_kwh: Option<String>,
    pub interval_seconds: Option<u64>,
    pub repeat_after_seconds: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct AccountStatus {
    pub state: String,
    pub preferences: Preferences,
    pub last_check_at: Option<i64>,
    pub next_check_at: i64,
}
#[derive(Serialize)]
pub struct HistoryEntry {
    pub checked_at: String,
    pub remaining_kwh: Option<String>,
    pub supply_status: Option<String>,
    pub error: Option<String>,
}

/// Only the authenticated actor's own data can be selected. File paths, school
/// accounts, recipients and other user IDs are not accepted in these commands.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UserRequest {
    Status {},
    Query {},
    History { limit: u32 },
    Preferences { patch: PreferencePatch },
    Unbind {},
}

pub struct SchoolReading {
    pub reading: Reading,
    pub location: Location,
}
pub trait ElectricityBackend: Sync {
    fn read(&self, config: &Path, timeout: Duration) -> Result<SchoolReading, QueryError>;
}
pub struct SchoolBackend;
impl ElectricityBackend for SchoolBackend {
    fn read(&self, path: &Path, timeout: Duration) -> Result<SchoolReading, QueryError> {
        crate::with_dorm_auth(path, timeout, |config, client| {
            if config.form.values().any(|v| v == "_") {
                return Err(QueryError::Config("请完成宿舍目标设置。"));
            }
            Ok(SchoolReading {
                reading: crate::query(config, client)?,
                location: config.location()?,
            })
        })
    }
}

#[derive(Clone)]
pub struct Service {
    root: PathBuf,
    timeout: Duration,
}
#[derive(Default, Serialize)]
pub struct TickReport {
    pub checked: u32,
    pub failed: u32,
    pub skipped: u32,
}

impl Service {
    pub fn open(root: &Path, timeout: Duration) -> Result<Self, ServiceError> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(ServiceError::Invalid("网络超时须为 1 到 300 秒。"));
        }
        store::directory(root)?;
        let service = Self {
            root: fs::canonicalize(root).map_err(|_| ServiceError::Storage)?,
            timeout,
        };
        store::directory(&service.root.join("users"))?;
        store::directory(&service.root.join("locks"))?;
        let _lock = store::lock(&service.root.join("registry.lock"))?;
        service.registry()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS bindings (
                id TEXT PRIMARY KEY, bot_id TEXT NOT NULL, user_id TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'active', UNIQUE(bot_id, user_id)
            );",
        )?;
        service.cleanup_deletions()?;
        Ok(service)
    }
    fn registry(&self) -> Result<Connection, ServiceError> {
        store::database(&self.root.join("registry.sqlite3"))
    }
    fn binding(&self, actor: &Actor) -> Result<Option<String>, ServiceError> {
        Ok(self
            .registry()?
            .query_row(
                "SELECT id FROM bindings WHERE bot_id=?1 AND user_id=?2 AND state='active'",
                params![actor.bot_id, actor.user_id],
                |row| row.get(0),
            )
            .optional()?)
    }
    fn guard(&self, actor: &Actor) -> Result<Guard, ServiceError> {
        let id = self.binding(actor)?.ok_or(ServiceError::NotBound)?;
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ServiceError::Storage);
        }
        let lock = store::lock(&self.root.join("locks").join(format!("{id}.lock")))?;
        if self.binding(actor)?.as_deref() != Some(&id) {
            return Err(ServiceError::NotBound);
        }
        let dir = self.root.join("users").join(&id);
        store::existing_directory(&dir)?;
        Ok(Guard { dir, _lock: lock })
    }
    fn actors(&self, bot_id: Option<&str>) -> Result<Vec<Actor>, ServiceError> {
        let connection = self.registry()?;
        let mut statement = connection.prepare(
            "SELECT bot_id,user_id FROM bindings WHERE state='active' AND (?1 IS NULL OR bot_id=?1) ORDER BY id",
        )?;
        let rows = statement.query_map([bot_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (bot, user) = row?;
            Actor::qq(&bot, &user)
        })
        .collect()
    }

    /// Administrator-only provisioning. Never expose this method to chat commands.
    /// Passwords are read from a local file, not command-line arguments or messages.
    pub fn provision(&self, actor: &Actor, source: &Path) -> Result<AccountStatus, ServiceError> {
        let mut settings = Settings::load(source)?;
        settings
            .cas
            .as_ref()
            .ok_or(ServiceError::Invalid("绑定配置须提供 CAS 账号。"))?
            .validate()?;
        if settings.dorm_electricity.building.is_none() || settings.dorm_electricity.room.is_none()
        {
            return Err(ServiceError::Invalid("绑定配置须提供宿舍楼栋和房间。"));
        }
        // No credential path, shared cache or device fingerprint from an input
        // file is carried across the tenant boundary.
        settings.auth.cache = Some("auth.json".into());
        settings.cas.as_mut().expect("validated CAS").device_id = Some(fresh_id());
        let _lock = store::lock(&self.root.join("registry.lock"))?;
        self.cleanup_deletions()?;
        if self.binding(actor)?.is_some() {
            return Err(ServiceError::AlreadyBound);
        }
        let id = fresh_id();
        let dir = self.root.join("users").join(&id);
        self.registry()?.execute(
            "INSERT INTO bindings (id,bot_id,user_id,state) VALUES (?1,?2,?3,'provisioning')",
            params![id, actor.bot_id, actor.user_id],
        )?;
        let result = (|| {
            store::directory(&dir)?;
            auth::secure_write(
                &dir.join("config.yaml"),
                &serde_yaml::to_string(&settings)
                    .map_err(|_| ServiceError::Storage)?
                    .into_bytes(),
            )?;
            let guard = Guard {
                dir: dir.clone(),
                _lock: store::lock(&self.root.join("locks").join(format!("{id}.lock")))?,
            };
            let connection = guard.database()?;
            guard.save_profile(&connection, &Profile::default())?;
            self.registry()?.execute(
                "UPDATE bindings SET state='active' WHERE id=?1 AND state='provisioning'",
                [&id],
            )?;
            Ok(Profile::default().status())
        })();
        if result.is_err() {
            let _ = self.cleanup_deletions();
        }
        result
    }
    pub fn status(&self, actor: &Actor) -> Result<AccountStatus, ServiceError> {
        let guard = self.guard(actor)?;
        Ok(guard.profile(&guard.database()?)?.status())
    }
    pub fn preferences(
        &self,
        actor: &Actor,
        patch: PreferencePatch,
    ) -> Result<AccountStatus, ServiceError> {
        let guard = self.guard(actor)?;
        let mut connection = guard.database()?;
        let mut profile = guard.profile(&connection)?;
        let old = serde_json::to_string(&profile.preferences).map_err(|_| ServiceError::Storage)?;
        let prefs = &mut profile.preferences;
        if let Some(value) = patch.enabled {
            prefs.enabled = value;
        }
        if let Some(value) = patch.threshold_kwh {
            prefs.threshold_kwh = value;
        }
        if let Some(value) = patch.interval_seconds {
            prefs.interval_seconds = value;
        }
        if let Some(value) = patch.repeat_after_seconds {
            prefs.repeat_after_seconds = value;
        }
        prefs.validate()?;
        prefs.threshold_kwh = prefs.threshold()?.normalize().to_string();
        let transaction = connection.transaction()?;
        if old != serde_json::to_string(prefs).map_err(|_| ServiceError::Storage)? {
            store::cancel(&transaction, "low")?;
            crate::clear_alert(&transaction, "low")?;
            profile.next_check_at = 0;
        }
        if !profile.preferences.enabled {
            store::cancel(&transaction, "auth")?;
            crate::clear_alert(&transaction, "auth")?;
        }
        guard.save_profile(&transaction, &profile)?;
        transaction.commit()?;
        Ok(profile.status())
    }
    pub fn query(&self, actor: &Actor) -> Result<Event, ServiceError> {
        self.query_with(actor, &SchoolBackend, Utc::now().timestamp())
    }
    pub fn query_with(
        &self,
        actor: &Actor,
        backend: &impl ElectricityBackend,
        now: i64,
    ) -> Result<Event, ServiceError> {
        let guard = self.guard(actor)?;
        let profile = guard.profile(&guard.database()?)?;
        if profile
            .last_manual_at
            .is_some_and(|last| now.saturating_sub(last) < 10)
        {
            return Err(ServiceError::RateLimited);
        }
        self.check(&guard, backend, now, true)
    }
    fn check(
        &self,
        guard: &Guard,
        backend: &impl ElectricityBackend,
        now: i64,
        manual: bool,
    ) -> Result<Event, ServiceError> {
        let mut connection = guard.database()?;
        let mut profile = guard.profile(&connection)?;
        if manual {
            profile.last_manual_at = Some(now);
            guard.save_profile(&connection, &profile)?;
        }
        let result = backend.read(&guard.dir.join("config.yaml"), self.timeout);
        let needs_auth = matches!(
            result,
            Err(QueryError::Authentication | QueryError::AuthFlow(_))
        );
        let invalid_target = matches!(result, Err(QueryError::Config(_)));
        let event = match result {
            Ok(reading) => Event::success(
                reading.reading,
                profile
                    .preferences
                    .enabled
                    .then(|| profile.preferences.threshold())
                    .transpose()?,
                None,
            )
            .at_location(reading.location),
            Err(ref error) => {
                let mut event = Event::failure(error);
                if matches!(error, QueryError::Authentication | QueryError::AuthFlow(_)) {
                    event.error = Some("学校登录需要重新验证，请完成绑定验证流程。".into());
                }
                event
            }
        };
        profile.last_check_at = Some(now);
        let transaction = connection.transaction()?;
        crate::save_event(&transaction, &event)?;
        if event.error.is_none() {
            profile.state = "active".into();
            profile.next_check_at = now.saturating_add(profile.preferences.interval_seconds as i64);
            store::cancel(&transaction, "auth")?;
            crate::clear_alert(&transaction, "auth")?;
            match event.low_balance {
                Some(true) => store::enqueue(
                    &transaction,
                    "low",
                    &format!(
                        "宿舍剩余电量 {} 度，提醒阈值 {} 度。查询时间：{}",
                        event.remaining_kwh.as_deref().unwrap_or("未知"),
                        profile.preferences.threshold_kwh,
                        event.checked_at
                    ),
                    now,
                    profile.preferences.repeat_after_seconds,
                )?,
                Some(false) | None => {
                    store::cancel(&transaction, "low")?;
                    crate::clear_alert(&transaction, "low")?;
                }
            }
        } else {
            if needs_auth {
                profile.state = "needs_auth".into();
                store::cancel(&transaction, "low")?;
                if profile.preferences.enabled {
                    store::enqueue(
                        &transaction,
                        "auth",
                        "电费监控的学校登录需要重新验证，请完成绑定验证流程；验证前已暂停自动查询。",
                        now,
                        profile.preferences.repeat_after_seconds,
                    )?;
                }
            } else if invalid_target {
                profile.state = "invalid_target".into();
                store::cancel(&transaction, "low")?;
            }
            profile.next_check_at = now.saturating_add(300);
        }
        guard.save_profile(&transaction, &profile)?;
        transaction.commit()?;
        Ok(event)
    }
    pub fn tick(&self) -> Result<TickReport, ServiceError> {
        self.tick_with(&SchoolBackend, Utc::now().timestamp())
    }
    pub fn tick_with(
        &self,
        backend: &impl ElectricityBackend,
        now: i64,
    ) -> Result<TickReport, ServiceError> {
        let mut report = TickReport::default();
        let actors = self.actors(None)?;
        for batch in actors.chunks(4) {
            let results = std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|actor| {
                        scope.spawn(move || {
                            let guard = self.guard(actor)?;
                            let profile = guard.profile(&guard.database()?)?;
                            if profile.state != "active"
                                || !profile.preferences.enabled
                                || profile.next_check_at > now
                            {
                                return Ok(None);
                            }
                            self.check(&guard, backend, now, false).map(Some)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|handle| handle.join().unwrap_or(Err(ServiceError::Storage)))
                    .collect::<Vec<_>>()
            });
            for result in results {
                match result {
                    Ok(Some(event)) => {
                        report.checked += 1;
                        if event.error.is_some() {
                            report.failed += 1;
                        }
                    }
                    Ok(None) | Err(ServiceError::Busy | ServiceError::NotBound) => {
                        report.skipped += 1
                    }
                    Err(_) => report.failed += 1,
                }
            }
        }
        Ok(report)
    }
    pub fn login(
        &self,
        actor: &Actor,
        options: auth::LoginOptions,
    ) -> Result<auth::AuthStatus, ServiceError> {
        let guard = self.guard(actor)?;
        let status = auth::login(&guard.dir.join("config.yaml"), self.timeout, options)?;
        self.reset_after_auth(&guard)?;
        Ok(status)
    }
    pub fn import_auth(
        &self,
        actor: &Actor,
        input: &Path,
        provider: auth::OAuthProvider,
        client_auth: Option<&Path>,
    ) -> Result<auth::AuthStatus, ServiceError> {
        let guard = self.guard(actor)?;
        let status = auth::import(&guard.dir.join("config.yaml"), input, provider, client_auth)?;
        self.reset_after_auth(&guard)?;
        Ok(status)
    }
    fn reset_after_auth(&self, guard: &Guard) -> Result<(), ServiceError> {
        let mut connection = guard.database()?;
        let mut profile = guard.profile(&connection)?;
        profile.state = "pending".into();
        profile.last_manual_at = None;
        let transaction = connection.transaction()?;
        for topic in ["low", "auth"] {
            store::cancel(&transaction, topic)?;
            crate::clear_alert(&transaction, topic)?;
        }
        guard.save_profile(&transaction, &profile)?;
        transaction.commit()?;
        Ok(())
    }
    pub fn history(&self, actor: &Actor, limit: u32) -> Result<Vec<HistoryEntry>, ServiceError> {
        if !(1..=1000).contains(&limit) {
            return Err(ServiceError::Invalid("历史条数须为 1 到 1000。"));
        }
        let guard = self.guard(actor)?;
        let connection = guard.database()?;
        let mut statement = connection.prepare("SELECT checked_at,remaining_kwh,supply_status,error FROM readings ORDER BY id DESC LIMIT ?1")?;
        Ok(statement
            .query_map([limit], |row| {
                Ok(HistoryEntry {
                    checked_at: row.get(0)?,
                    remaining_kwh: row.get(1)?,
                    supply_status: row.get(2)?,
                    error: row.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }
    pub fn unbind(&self, actor: &Actor) -> Result<(), ServiceError> {
        let _registry_lock = store::lock(&self.root.join("registry.lock"))?;
        let guard = self.guard(actor)?;
        self.registry()?.execute(
            "UPDATE bindings SET state='deleting' WHERE bot_id=?1 AND user_id=?2",
            params![actor.bot_id, actor.user_id],
        )?;
        fs::remove_dir_all(&guard.dir).map_err(|_| ServiceError::Storage)?;
        self.registry()?.execute(
            "DELETE FROM bindings WHERE bot_id=?1 AND user_id=?2",
            params![actor.bot_id, actor.user_id],
        )?;
        Ok(())
    }
    // Interrupted provisioning/unbinding leaves an inaccessible tombstone,
    // cleaned before QQ identity reuse. Caller holds the registry lock.
    fn cleanup_deletions(&self) -> Result<(), ServiceError> {
        let connection = self.registry()?;
        let ids = {
            let mut statement = connection
                .prepare("SELECT id FROM bindings WHERE state IN ('deleting','provisioning')")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for id in ids {
            if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(ServiceError::Storage);
            }
            let _lock = match store::lock(&self.root.join("locks").join(format!("{id}.lock"))) {
                Ok(lock) => lock,
                Err(ServiceError::Busy) => continue,
                Err(error) => return Err(error),
            };
            let path = self.root.join("users").join(&id);
            if path.exists() {
                fs::remove_dir_all(path).map_err(|_| ServiceError::Storage)?;
            }
            connection.execute(
                "DELETE FROM bindings WHERE id=?1 AND state IN ('deleting','provisioning')",
                [id],
            )?;
        }
        Ok(())
    }
    pub fn handle(
        &self,
        actor: &Actor,
        request: UserRequest,
    ) -> Result<serde_json::Value, ServiceError> {
        fn value(data: impl Serialize) -> Result<serde_json::Value, ServiceError> {
            serde_json::to_value(data).map_err(|_| ServiceError::Storage)
        }
        match request {
            UserRequest::Status {} => value(self.status(actor)?),
            UserRequest::Query {} => value(self.query(actor)?),
            UserRequest::History { limit } => value(self.history(actor, limit)?),
            UserRequest::Preferences { patch } => value(self.preferences(actor, patch)?),
            UserRequest::Unbind {} => {
                self.unbind(actor)?;
                Ok(serde_json::json!({"unbound":true}))
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Delivery {
    pub actor: Actor,
    pub notification_id: String,
    pub lease_token: String,
    pub message: String,
    pub attempts: u32,
}
#[derive(Serialize)]
pub struct PendingNotification {
    pub id: String,
    pub topic: String,
    pub message: String,
    pub state: String,
    pub attempts: u32,
}

/// Future NapCat adapter implements this boundary. No implementation here sends QQ messages.
pub trait NotificationSink {
    fn send_private(&mut self, delivery: &Delivery) -> Result<String, SendError>;
}
pub struct SendError;

impl Service {
    pub fn outbox(&self, actor: &Actor) -> Result<Vec<PendingNotification>, ServiceError> {
        let guard = self.guard(actor)?;
        let connection = guard.database()?;
        let mut statement = connection.prepare("SELECT id,topic,message,state,attempts FROM outbox WHERE state IN ('pending','leased') ORDER BY created_at LIMIT 100")?;
        Ok(statement
            .query_map([], |row| {
                Ok(PendingNotification {
                    id: row.get(0)?,
                    topic: row.get(1)?,
                    message: row.get(2)?,
                    state: row.get(3)?,
                    attempts: row.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    }
    /// Administrator/transport-only API. Recipients are read from the registry.
    pub fn claim_deliveries(
        &self,
        bot_id: &str,
        limit: u32,
        now: i64,
    ) -> Result<Vec<Delivery>, ServiceError> {
        if !(1..=100).contains(&limit) {
            return Err(ServiceError::Invalid("每批发送条数须为 1 到 100。"));
        }
        let mut deliveries = Vec::new();
        for actor in self.actors(Some(bot_id))? {
            let guard = match self.guard(&actor) {
                Ok(guard) => guard,
                Err(ServiceError::Busy | ServiceError::NotBound) => continue,
                Err(error) => return Err(error),
            };
            let mut connection = guard.database()?;
            let transaction = connection.transaction()?;
            transaction.execute("UPDATE outbox SET state='cancelled',lease_token=NULL WHERE state IN ('pending','leased') AND created_at<?1", [now.saturating_sub(86400)])?;
            let entry: Option<(String,String,u32)> = transaction.query_row(
                "SELECT id,message,attempts FROM outbox WHERE (state='pending' AND retry_at<=?1) OR (state='leased' AND lease_until<=?1) ORDER BY created_at LIMIT 1",
                [now], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            if let Some((id, message, attempts)) = entry {
                let lease_token = fresh_id();
                transaction.execute("UPDATE outbox SET state='leased',lease_token=?1,lease_until=?2,attempts=attempts+1 WHERE id=?3", params![lease_token,now.saturating_add(120),id])?;
                deliveries.push(Delivery {
                    actor,
                    notification_id: id,
                    lease_token,
                    message,
                    attempts: attempts + 1,
                });
            }
            transaction.commit()?;
            if deliveries.len() >= limit as usize {
                break;
            }
        }
        Ok(deliveries)
    }
    /// A stale or cross-user ACK cannot mark a notification delivered.
    pub fn complete_delivery(
        &self,
        delivery: &Delivery,
        sent: bool,
        now: i64,
    ) -> Result<(), ServiceError> {
        let guard = self.guard(&delivery.actor)?;
        self.complete_locked(&guard, delivery, sent, now)
    }
    fn complete_locked(
        &self,
        guard: &Guard,
        delivery: &Delivery,
        sent: bool,
        now: i64,
    ) -> Result<(), ServiceError> {
        let mut connection = guard.database()?;
        let transaction = connection.transaction()?;
        let topic: Option<String> = transaction.query_row("SELECT topic FROM outbox WHERE id=?1 AND state='leased' AND lease_token=?2 AND lease_until>?3", params![delivery.notification_id,delivery.lease_token,now], |row| row.get(0)).optional()?;
        let topic = topic.ok_or(ServiceError::InvalidLease)?;
        if sent {
            transaction.execute(
                "UPDATE outbox SET state='sent',lease_token=NULL WHERE id=?1",
                [&delivery.notification_id],
            )?;
            crate::mark_alert(&transaction, &topic, now)?;
        } else {
            transaction.execute(
                "UPDATE outbox SET state='pending',lease_token=NULL,retry_at=?1 WHERE id=?2",
                params![now.saturating_add(60), delivery.notification_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
    /// Holds each tenant's lock through the send, so unbinding/recovery cannot
    /// race with a queued private message. A crash can cause a duplicate send;
    /// delivery IDs let a future transport deduplicate where supported.
    pub fn dispatch(
        &self,
        bot_id: &str,
        sink: &mut impl NotificationSink,
    ) -> Result<u32, ServiceError> {
        let mut sent = 0;
        for _ in 0..100 {
            let now = Utc::now().timestamp();
            let Some(delivery) = self.claim_deliveries(bot_id, 1, now)?.pop() else {
                break;
            };
            let guard = match self.guard(&delivery.actor) {
                Ok(guard) => guard,
                Err(ServiceError::Busy | ServiceError::NotBound) => continue,
                Err(error) => return Err(error),
            };
            let connection = guard.database()?;
            let valid: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM outbox WHERE id=?1 AND state='leased' AND lease_token=?2 AND lease_until>?3)", params![delivery.notification_id,delivery.lease_token,now], |row| row.get(0))?;
            if !valid {
                continue;
            }
            let success = sink.send_private(&delivery).is_ok();
            self.complete_locked(&guard, &delivery, success, Utc::now().timestamp())?;
            if success {
                sent += 1;
            }
        }
        Ok(sent)
    }
}
