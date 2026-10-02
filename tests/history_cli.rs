//! Stored records and CLI output remain compatible with pre-location databases.
use sdu_infohelper::{Event, Location, QueryError, storage::HistoryStore};
use std::{fs, process::Command};

#[test]
fn store_and_cli_preserve_legacy_rows_location_and_failure_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE readings (id INTEGER PRIMARY KEY, checked_at TEXT NOT NULL, remaining_kwh TEXT, supply_status TEXT, threshold_kwh TEXT, low_balance INTEGER, error TEXT);
            INSERT INTO readings (checked_at,remaining_kwh,supply_status) VALUES ('legacy','12.30','正常');").unwrap();
    }
    let store = HistoryStore::open(&path).unwrap();
    let mut failed = Event::failure(&QueryError::Timeout).at_location(Location {
        campus: "C&主校区".into(),
        building: "B&8".into(),
        floor: "F&4".into(),
        room: "R&405".into(),
    });
    failed.checked_at = "new".into();
    store.record(&failed).unwrap();
    let latest = store.recent(1).unwrap();
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].checked_at, "new");
    assert!(latest[0].remaining_kwh.is_none());
    assert_eq!(latest[0].location.as_ref().unwrap().room, "R&405");
    drop(store);
    let output = Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"))
        .args(["history", "--history"])
        .arg(&path)
        .args(["--limit", "2"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "new\t主校区 / 8栋 / 4层 / 405号\t查询失败\t连接学校接口超时，未获得有效电量。\nlegacy\t宿舍未记录\t12.30 度\t正常\n"
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn missing_history_remains_an_error_without_creating_database_or_directories() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent/history.sqlite3");
    let output = Command::new(env!("CARGO_BIN_EXE_sdu-infohelper"))
        .args(["history", "--history"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "尚无历史记录，请先查询一次。\n"
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}
