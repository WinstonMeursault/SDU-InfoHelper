//! Explicit test notifications, isolated from monitor history and cooldowns.
use crate::{monitor::delivery, notification::Alert, settings::Settings};
use std::path::Path;

pub fn test_notification(config: &Path, selected: Option<&str>) -> Result<bool, String> {
    let settings = Settings::read(config).map_err(|e| e.to_string())?;
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
