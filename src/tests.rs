use rusqlite::Connection;
use rust_decimal::Decimal;
use serde_json::Value;
use std::{collections::BTreeMap, fs, str::FromStr, time::Duration};

use super::*;
use serde_json::json;

fn payload(info: &str) -> Value {
    json!({"code": 200, "map": {"showData": {"信息": info}}})
}

fn config() -> Config {
    serde_json::from_value(json!({
            "schema_version": 1, "url": ENDPOINT,
            "form": {"feeitemid":"411", "type":"IEC", "level":"4", "campus":"C1&Campus", "building":"B1&Building", "floor":"F1&Floor", "room":"R1&Room"},
            "headers": {"synjones-auth": "test-token"}
        })).unwrap()
}

#[test]
fn temporary_targets_are_merged_before_stale_defaults_are_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let original = b"dorm_electricity:\n  campus: C1&Campus\n  building: B1&Building\n  floor: removed-floor\n  room: removed-room\n";
    auth::secure_write(&path, original).unwrap();
    let settings = settings::Settings::load(&path).unwrap();
    auth::secure_write(
            &settings.cache_path(&path),
            br#"{"access_token":"test","refresh_token":null,"expires_at":null,"client_authorization":null}"#,
        )
        .unwrap();
    let overrides = BTreeMap::from([
        ("floor".into(), "F2&Floor".into()),
        ("room".into(), "R2&Room".into()),
    ]);
    with_dorm_auth_overrides(&path, Duration::from_secs(1), &overrides, |request, _| {
        assert_eq!(request.form["floor"], "F2&Floor");
        assert_eq!(request.form["room"], "R2&Room");
        Ok(())
    })
    .unwrap();
    let overrides = BTreeMap::from([("floor".into(), "F2&Floor".into())]);
    with_dorm_directory_auth_overrides(
        &path,
        Duration::from_secs(1),
        SelectionLevel::Rooms,
        &overrides,
        |request, _| {
            assert_eq!(request.form["floor"], "F2&Floor");
            assert_eq!(request.form["room"], "_");
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn legacy_requests_keep_query_and_directory_override_rules() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("request.json");
    fs::write(&path, serde_json::to_vec(&config()).unwrap()).unwrap();
    let overrides = BTreeMap::from([
        ("building".into(), "B2&Building".into()),
        ("floor".into(), "F2&Floor".into()),
        ("room".into(), "R2&Room".into()),
    ]);
    with_dorm_auth_overrides(&path, Duration::from_secs(1), &overrides, |request, _| {
        assert_eq!(request.form["building"], "B2&Building");
        assert_eq!(request.form["room"], "R2&Room");
        Ok(())
    })
    .unwrap();
    let incomplete = BTreeMap::from([("building".into(), "B2&Building".into())]);
    assert!(matches!(
        with_dorm_auth_overrides(&path, Duration::from_secs(1), &incomplete, |_, _| Ok(())),
        Err(QueryError::Config(_))
    ));
    with_dorm_directory_auth_overrides(
        &path,
        Duration::from_secs(1),
        SelectionLevel::Floors,
        &incomplete,
        |request, _| {
            let form = selection_form(request, SelectionLevel::Floors, &BTreeMap::new())?;
            assert_eq!(form["building"], "B2&Building");
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn directory_forms_include_only_required_parents() {
    let config = config();
    for (level, number) in [
        (SelectionLevel::Campuses, 0),
        (SelectionLevel::Buildings, 1),
        (SelectionLevel::Floors, 2),
        (SelectionLevel::Rooms, 3),
    ] {
        let form = selection_form(&config, level, &BTreeMap::new()).unwrap();
        assert_eq!(form.len(), 3 + number);
        assert_eq!(form["type"], "select");
        assert_eq!(form["level"], number.to_string());
        assert!(!form.contains_key("room"));
    }
    assert_eq!(config.form["type"], "IEC");
}

#[test]
fn changing_building_requires_new_floor_and_room() {
    let config = config();
    let mut overrides = BTreeMap::from([("building".into(), "B2&Other".into())]);
    assert!(config.with_location_overrides(&overrides).is_err());
    overrides.insert("floor".into(), "F2&Other".into());
    overrides.insert("room".into(), "R2&Other".into());
    let changed = config.with_location_overrides(&overrides).unwrap();
    assert_eq!(changed.form["room"], "R2&Other");
    assert_eq!(config.form["room"], "R1&Room");
}

#[test]
fn another_room_on_same_floor_can_be_selected_alone() {
    let config = config();
    let changed = config
        .with_location_overrides(&BTreeMap::from([("room".into(), "R2&Other".into())]))
        .unwrap();
    assert_eq!(changed.form["floor"], config.form["floor"]);
    assert_eq!(changed.form["room"], "R2&Other");
}

#[test]
fn listing_rooms_in_new_building_requires_a_floor() {
    let config = config();
    let mut overrides = BTreeMap::from([("building".into(), "B2&Other".into())]);
    assert!(selection_form(&config, SelectionLevel::Rooms, &overrides).is_err());
    overrides.insert("floor".into(), "F2&Other".into());
    let form = selection_form(&config, SelectionLevel::Rooms, &overrides).unwrap();
    assert_eq!(form["floor"], "F2&Other");
}

#[test]
fn directory_values_are_preserved_and_malformed_data_is_rejected() {
    let options = parse_selection_options(
        &json!({"code":200,"map":{"data":[{"name":"202","value":"R2&202"}]}}),
    )
    .unwrap();
    assert_eq!(
        options,
        vec![SelectionOption {
            name: "202".into(),
            value: "R2&202".into()
        }]
    );
    assert!(parse_selection_options(&json!({"code":200,"map":{"data":{}}})).is_err());
    assert!(matches!(
        parse_selection_options(&json!({"code":401})),
        Err(QueryError::Authentication)
    ));
}

#[test]
fn a_valid_energy_response_for_the_wrong_room_is_rejected() {
    let mut response = payload("剩余电量为12.34度");
    response["map"]["data"] = json!({"room":"R2"});
    assert!(validate_room_echo(&response, "R2&202").is_ok());
    assert!(validate_room_echo(&response, "R1&101").is_err());
    assert!(validate_room_echo(&payload("剩余电量为12.34度"), "R2&202").is_err());
}

#[test]
fn correct_room_with_wrong_parent_location_is_rejected() {
    let config = config();
    let mut response = payload("剩余电量为12.34度");
    response["map"]["data"] = json!({"area":"C1","building":"B1","floor":"F1","room":"R1"});
    assert!(validate_location_echo(&response, &config).is_ok());
    response["map"]["data"]["floor"] = json!("F2");
    assert!(validate_location_echo(&response, &config).is_err());
}

#[test]
fn legacy_history_is_migrated_and_new_rooms_are_recorded_separately() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    {
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE readings (id INTEGER PRIMARY KEY, checked_at TEXT NOT NULL, remaining_kwh TEXT, supply_status TEXT, threshold_kwh TEXT, low_balance INTEGER, error TEXT); INSERT INTO readings (checked_at,remaining_kwh) VALUES ('legacy','35.67');").unwrap();
    }
    let connection = history(&path).unwrap();
    let config = config();
    for room in ["R1&101", "R2&202"] {
        let changed = config
            .with_location_overrides(&BTreeMap::from([("room".into(), room.into())]))
            .unwrap();
        let event = Event::success(
            parse_response(&payload("剩余电量为12.34度")).unwrap(),
            None,
            None,
        )
        .at_location(changed.location().unwrap());
        save_event(&connection, &event).unwrap();
    }
    let mut statement = connection
        .prepare("SELECT room FROM readings ORDER BY id")
        .unwrap();
    let rooms: Vec<Option<String>> = statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rooms,
        vec![None, Some("R1&101".into()), Some("R2&202".into())]
    );
}

#[test]
fn concurrent_history_initialization_and_migration_preserve_all_writes() {
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    let dir = tempfile::tempdir().unwrap();
    for legacy in [false, true] {
        for attempt in 0..4 {
            let path = dir
                .path()
                .join(format!("history-{legacy}-{attempt}.sqlite3"));
            if legacy {
                let connection = Connection::open(&path).unwrap();
                connection.execute_batch("CREATE TABLE readings (id INTEGER PRIMARY KEY, checked_at TEXT NOT NULL, remaining_kwh TEXT, supply_status TEXT, threshold_kwh TEXT, low_balance INTEGER, error TEXT); INSERT INTO readings (checked_at, remaining_kwh) VALUES ('legacy', '35.67');").unwrap();
            }
            let start = Arc::new(Barrier::new(8));
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    let start = Arc::clone(&start);
                    let path = path.clone();
                    thread::spawn(move || -> rusqlite::Result<()> {
                        start.wait();
                        let connection = history(&path)?;
                        save_event(
                            &connection,
                            &Event::failure(&QueryError::Network)
                                .at_location(config().location().unwrap()),
                        )
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap().unwrap();
            }
            let connection = history(&path).unwrap();
            let writes: u32 = connection.query_row("SELECT COUNT(*) FROM readings WHERE campus = 'C1&Campus' AND building = 'B1&Building' AND floor = 'F1&Floor' AND room = 'R1&Room'", [], |row| row.get(0)).unwrap();
            assert_eq!(writes, 8);
            if legacy {
                let value: String = connection
                    .query_row(
                        "SELECT remaining_kwh FROM readings WHERE checked_at = 'legacy'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(value, "35.67");
            }
        }
    }
}

#[test]
fn valid_energy_survives_failed_supply_status() {
    let reading = parse_response(&payload("剩余电量为35.67度，供电状态：查询失败   ")).unwrap();
    assert_eq!(reading.remaining_kwh, Decimal::from_str("35.67").unwrap());
    assert_eq!(reading.supply_status.as_deref(), Some("查询失败"));
}

#[test]
fn unavailable_energy_is_not_zero() {
    assert!(parse_response(&payload("剩余电量查询失败")).is_err());
    assert!(Event::failure(&QueryError::Timeout).remaining_kwh.is_none());
}

#[test]
fn zero_and_negative_readings_are_valid() {
    for value in ["0", "-1.23"] {
        let reading = parse_response(&payload(&format!("剩余电量为{value}度"))).unwrap();
        assert_eq!(reading.remaining_kwh, Decimal::from_str(value).unwrap());
    }
}

#[test]
fn expired_login_is_distinct_from_parse_failure() {
    assert!(matches!(
        parse_response(&json!({"code": 401})),
        Err(QueryError::Authentication)
    ));
    assert!(matches!(
        parse_response(&json!({"code": "403"})),
        Err(QueryError::Authentication)
    ));
}

#[test]
fn threshold_is_inclusive_and_exact() {
    let reading = Reading {
        remaining_kwh: Decimal::from_str("10.00").unwrap(),
        supply_status: None,
    };
    let event = Event::success(reading, Some(Decimal::from(10)), None);
    assert_eq!(event.low_balance, Some(true));
}

#[test]
fn failed_event_is_stored_as_null_energy() {
    let directory = tempfile::tempdir().unwrap();
    let connection = history(&directory.path().join("history.sqlite3")).unwrap();
    save_event(&connection, &Event::failure(&QueryError::Authentication)).unwrap();
    let energy: Option<String> = connection
        .query_row("SELECT remaining_kwh FROM readings", [], |row| row.get(0))
        .unwrap();
    assert!(energy.is_none());
}

#[test]
fn cooldown_survives_restart_and_resets_after_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    {
        let connection = history(&path).unwrap();
        assert!(alert_due(&connection, "low", 100, 60).unwrap());
        mark_alert(&connection, "low", 100).unwrap();
    }
    let connection = history(&path).unwrap();
    assert!(!alert_due(&connection, "low", 159, 60).unwrap());
    assert!(alert_due(&connection, "low", 160, 60).unwrap());
    clear_alert(&connection, "low").unwrap();
    assert!(alert_due(&connection, "low", 159, 60).unwrap());
}

#[test]
fn payment_actions_and_other_hosts_are_rejected() {
    let mut config: Config = serde_json::from_value(json!({
            "schema_version": 1, "url": ENDPOINT,
            "form": {"feeitemid":"411", "type":"IEC", "level":"4", "campus":"test", "building":"test", "floor":"test", "room":"test"},
            "headers": {"synjones-auth": "test"}
        })).unwrap();
    assert!(config.validate().is_ok());
    config.form.insert("type".into(), "pay".into());
    assert!(config.validate().is_err());
    config.form.insert("type".into(), "IEC".into());
    config.url = "https://example.invalid".into();
    assert!(config.validate().is_err());
}
