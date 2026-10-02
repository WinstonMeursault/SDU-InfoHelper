//! Synchronous worker orchestration and graceful cancellation.
use super::{
    Log, Paths, RunOptions,
    instance::{private_directory, random_id},
    signals,
    state::{WorkerState, WorkerStatus},
};
use crate::{
    Event, QueryError,
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
    let mut state = WorkerState::new(
        run_id,
        Utc::now(),
        previous.and_then(|state| state.last_success),
        starts,
    );
    let result = (|| {
        if options.managed && state.recent_starts.len() >= 3 {
            return Err("监控短时间反复启动，已停止自动重启；请检查日志后重新 start。".into());
        }
        state.recent_starts.push(now);
        paths.save(&state.snapshot())?;
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
                state.status = WorkerStatus::Checking;
                state.next_check_at = chrono::DateTime::from_timestamp(schedule.next_wall(), 0);
                paths.save(&state.snapshot())?;
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
                let Some(observation) = monitor::observe_once_controlled(
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
                let event = Event::from(&observation);
                history
                    .record(&event)
                    .map_err(|_| "无法保存监控查询记录。")?;
                state.observe(&observation);
                let message = observation.message();
                log.write(&message)?;
                if !options.managed {
                    println!("[{0}] {message}", event.checked_at);
                }
                engine.observe_observation(&observation, Utc::now().timestamp())?;
                state.channels = engine.states()?;
                paths.save(&state.snapshot())?;
                let (fatal, needs_login) = observation.flags();
                if fatal && !needs_login {
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
                paths.save(&state.snapshot())?;
                continue;
            }
            thread::sleep(Duration::from_millis(250));
        }
        Ok(())
    })();
    state.finish(&result);
    paths.save(&state.snapshot())?;
    log.write(if result.is_ok() {
        "监控已停止。"
    } else {
        state.last_error.as_deref().unwrap_or("监控失败。")
    })?;
    result
}
