//! Legacy watch policy and output, preserved separately from daemon lifecycle.
use super::{
    args::WatchArgs,
    output::{alert, print_event},
    queries::event,
};
use chrono::Utc;
use rust_decimal::Decimal;
use std::{thread, time::Duration};

pub(super) fn run(mut args: WatchArgs) -> Result<bool, String> {
    args.query.threshold.get_or_insert(Decimal::from(10));
    let history = super::history::open_for_write(&args.query.history)?;
    loop {
        let (event, fatal, auth) = event(&args.query);
        history.record(&event).map_err(|_| "无法写入查询记录。")?;
        print_event(&event, args.query.json);
        let now = Utc::now().timestamp();
        let topic = format!(
            "low:{}:{}",
            event
                .location
                .as_ref()
                .map(|location| serde_json::to_string(location).expect("location serialization"))
                .unwrap_or_default(),
            event.threshold_kwh.as_deref().unwrap_or("")
        );
        if event.low_balance == Some(true) {
            if history
                .alert_due(&topic, now, args.repeat_after)
                .map_err(|_| "无法读取提醒记录。")?
                && alert(
                    "宿舍电量不足",
                    &format!(
                        "剩余 {} 度，阈值 {} 度。",
                        event.remaining_kwh.as_deref().unwrap_or("未知"),
                        event.threshold_kwh.as_deref().unwrap_or("未知")
                    ),
                    args.notify_desktop,
                )
            {
                history
                    .mark_alert(&topic, now)
                    .map_err(|_| "无法保存提醒记录。")?;
            }
        } else if event.low_balance == Some(false) {
            history
                .clear_alert(&topic)
                .map_err(|_| "无法更新提醒记录。")?;
        }
        if auth {
            alert(
                "电费监控登录已失效",
                "请在本机运行 auth login --trust-device 完成登录，然后重新启动监控。",
                args.notify_desktop,
            );
        }
        if fatal {
            return Ok(false);
        }
        thread::sleep(Duration::from_secs(args.interval));
    }
}

#[cfg(test)]
mod tests {
    use sdu_infohelper::QueryError;
    use sdu_infohelper::monitor;

    #[test]
    fn second_factor_requirement_stops_watch_and_requests_attention() {
        assert_eq!(
            monitor::failure_flags(&QueryError::AuthFlow("需要二次验证")),
            (true, true)
        );
        assert_eq!(monitor::failure_flags(&QueryError::Network), (false, false));
        for error in [
            QueryError::Http(408),
            QueryError::Http(429),
            QueryError::Response("刷新响应暂不可用"),
        ] {
            assert_eq!(monitor::failure_flags(&error), (false, false));
        }
    }
}
