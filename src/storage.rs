//! SQLite schema, readings and successful alert cooldowns.
use crate::Event;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{path::Path, time::Duration};

pub fn history(path: &Path) -> rusqlite::Result<Connection> {
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    // Acquire the write lock before inspecting the schema so concurrent callers
    // cannot both decide to add the same column.
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS readings (
        id INTEGER PRIMARY KEY, checked_at TEXT NOT NULL,
        remaining_kwh TEXT, supply_status TEXT, threshold_kwh TEXT,
        low_balance INTEGER, error TEXT
    ); CREATE TABLE IF NOT EXISTS alert_state (
        topic TEXT PRIMARY KEY, last_sent INTEGER NOT NULL
    );",
    )?;
    let columns: Vec<String> = {
        let mut statement = transaction.prepare("PRAGMA table_info(readings)")?;
        statement
            .query_map([], |row| row.get(1))?
            .collect::<Result<_, _>>()?
    };
    for column in ["campus", "building", "floor", "room"] {
        if !columns.iter().any(|existing| existing == column) {
            transaction.execute(
                &format!("ALTER TABLE readings ADD COLUMN {column} TEXT"),
                [],
            )?;
        }
    }
    transaction.commit()?;
    Ok(connection)
}

pub fn save_event(connection: &Connection, event: &Event) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO readings (checked_at, remaining_kwh, supply_status, threshold_kwh, low_balance, error, campus, building, floor, room) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![event.checked_at, event.remaining_kwh, event.supply_status,
            event.threshold_kwh, event.low_balance, event.error,
            event.location.as_ref().map(|location| &location.campus),
            event.location.as_ref().map(|location| &location.building),
            event.location.as_ref().map(|location| &location.floor),
            event.location.as_ref().map(|location| &location.room)],
    )?;
    Ok(())
}

pub fn alert_due(
    connection: &Connection,
    topic: &str,
    now: i64,
    repeat_after: u64,
) -> rusqlite::Result<bool> {
    let last: Option<i64> = connection
        .query_row(
            "SELECT last_sent FROM alert_state WHERE topic = ?1",
            [topic],
            |row| row.get(0),
        )
        .optional()?;
    Ok(last.is_none_or(|last| now.saturating_sub(last) >= repeat_after as i64))
}

pub fn mark_alert(connection: &Connection, topic: &str, now: i64) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO alert_state (topic, last_sent) VALUES (?1, ?2) \
        ON CONFLICT(topic) DO UPDATE SET last_sent = excluded.last_sent",
        params![topic, now],
    )?;
    Ok(())
}

pub fn clear_alert(connection: &Connection, topic: &str) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM alert_state WHERE topic = ?1", [topic])?;
    Ok(())
}
