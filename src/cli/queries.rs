//! Single queries and directory CLI commands.
use super::{
    args::{ListArgs, QueryArgs},
    output::{alert, print_event},
};
use sdu_infohelper::{
    Event, aircon, auth, monitor, selection_options, with_dorm_directory_auth_overrides,
};
use std::{collections::BTreeMap, path::Path, time::Duration};

pub(super) fn event(args: &QueryArgs) -> (Event, bool, bool) {
    let sample = monitor::query_once(
        &args.config,
        Duration::from_secs(args.timeout),
        args.threshold,
        &args.location.overrides(),
        monitor::Comparison::Inclusive,
    );
    (sample.event, sample.fatal, sample.needs_login)
}

pub(super) fn query(args: QueryArgs) -> Result<bool, String> {
    let history = super::history::open_for_write(&args.history)?;
    let (event, _, _) = event(&args);
    history.record(&event).map_err(|_| "无法写入查询记录。")?;
    print_event(&event, args.json);
    if event.low_balance == Some(true) {
        alert(
            "宿舍电量不足",
            &format!(
                "剩余 {} 度，阈值 {} 度。",
                event.remaining_kwh.as_deref().unwrap_or("未知"),
                event.threshold_kwh.as_deref().unwrap_or("未知")
            ),
            false,
        );
    }
    Ok(event.error.is_none())
}

pub(super) fn list(args: ListArgs) -> Result<bool, String> {
    let overrides = [
        ("campus", args.campus),
        ("building", args.building),
        ("floor", args.floor),
    ]
    .into_iter()
    .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
    .collect();
    let options = with_dorm_directory_auth_overrides(
        &args.config,
        Duration::from_secs(args.timeout),
        args.level,
        &overrides,
        |config, connection| selection_options(config, connection, args.level, &BTreeMap::new()),
    )
    .map_err(|error| error.to_string())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string(&options).map_err(|_| "目录序列化失败。")?
        );
    } else {
        println!("名称\t参数值");
        for option in options {
            println!("{}\t{}", option.name, option.value);
        }
    }
    Ok(true)
}

pub(super) fn aircon(
    config: &Path,
    timeout: u64,
    sms: bool,
    trust_device: bool,
    json: bool,
) -> Result<bool, String> {
    let reading = aircon::query_config(
        config,
        Duration::from_secs(timeout),
        auth::LoginOptions { sms, trust_device },
    )
    .map_err(|e| e.to_string())?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&reading).map_err(|_| "空调读数序列化失败。")?
        );
    } else {
        println!(
            "{} 公寓 / {} 层 / {} 房间：空调剩余 {} 度",
            reading.building, reading.floor, reading.room, reading.remaining_kwh
        );
    }
    Ok(true)
}
