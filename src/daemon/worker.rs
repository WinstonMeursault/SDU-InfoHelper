//! Synchronous worker orchestration and graceful cancellation.
use super::{
    LastSuccess, Log, Paths, RunOptions, RuntimeState,
    instance::{private_directory, random_id},
    signals,
};
use crate::{
    QueryError,
    control::OperationControl,
    monitor::{
        self, Comparison, Schedule,
        delivery::{self, DeliveryEngine},
    },
    settings::Settings,
    storage::HistoryStore,
};
use chrono::Utc;
use std::{
    collections::BTreeMap,
    path::Path,
    thread,
    time::{Duration, Instant},
};

fn timestamp(value: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(value, 0).map(|date| date.to_rfc3339())
}

fn secure_history(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::{fs, os::unix::fs::PermissionsExt};
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
        let settings = Settings::read(&paths.config).map_err(|e| e.to_string())?;
        let daemon = options.resolve(&settings, &paths.config)?;
        let channels = delivery::channels(&settings.notifications)?;
        if let Some(parent) = daemon.history.parent() {
            private_directory(parent)?;
        }
        let history =
            HistoryStore::open(&daemon.history).map_err(|_| "无法打开监控历史数据库。")?;
        secure_history(&daemon.history)?;
        let mut engine = DeliveryEngine::for_history(
            &history,
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
                let cancelled = || {
                    if signals::requested() {
                        return Ok(true);
                    }
                    paths
                        .stop_requested(&state.run_id)
                        .map_err(|_| QueryError::Config("无法读取停止请求。"))
                };
                let control = OperationControl::new(
                    Duration::from_secs(daemon.query_timeout_seconds),
                    &cancelled,
                );
                let Some(sample) = monitor::query_once_controlled(
                    &paths.config,
                    &control,
                    Some(daemon.threshold_kwh),
                    &BTreeMap::new(),
                    Comparison::StrictlyBelow,
                ) else {
                    break;
                };
                if signals::requested() || paths.stop_requested(&state.run_id)? {
                    break;
                }
                history
                    .record(&sample.event)
                    .map_err(|_| "无法保存监控查询记录。")?;
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
