//! Human-readable output and optional legacy Linux desktop notification.
use sdu_infohelper::Event;
use std::process::Command as ProcessCommand;

pub(super) fn print_event(event: &Event, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(event).expect("event serialization")
        );
    } else if let Some(error) = &event.error {
        eprintln!("[{}] 查询失败：{error}", event.checked_at);
    } else {
        if let Some(location) = &event.location {
            println!("宿舍：{}", location.label());
        }
        println!(
            "[{}] 剩余电量：{} 度",
            event.checked_at,
            event.remaining_kwh.as_deref().unwrap_or("未知")
        );
        if let Some(supply) = &event.supply_status {
            println!("供电状态（接口原文）：{supply}");
        }
    }
}

pub(super) fn alert(title: &str, message: &str, desktop: bool) -> bool {
    eprintln!("提醒：{title} — {message}");
    if !desktop {
        return true;
    }
    match ProcessCommand::new("notify-send")
        .args([
            "--app-name=SDU-InfoHelper",
            "--urgency=critical",
            title,
            message,
        ])
        .status()
    {
        Ok(status) if status.success() => true,
        _ => {
            eprintln!("桌面通知未发送成功，提醒已保留在终端日志。");
            false
        }
    }
}
