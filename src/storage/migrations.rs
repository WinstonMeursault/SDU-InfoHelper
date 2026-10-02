//! History schema migration is serialized before inspecting columns.
use rusqlite::{Connection, TransactionBehavior};
use std::{path::Path, time::Duration};

pub(super) fn open_history(path: &Path) -> rusqlite::Result<Connection> {
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

pub(super) fn prepare_delivery(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS daemon_delivery (
            instance TEXT NOT NULL, topic TEXT NOT NULL, channel TEXT NOT NULL,
            event_type TEXT NOT NULL, payload TEXT NOT NULL, attempts INTEGER NOT NULL,
            status TEXT NOT NULL, next_attempt INTEGER, round_started INTEGER NOT NULL,
            updated_at INTEGER NOT NULL, last_error TEXT,
            PRIMARY KEY (instance, topic, channel)
        );",
    )
}
