//! Reading storage and legacy successful-alert cooldowns.
use crate::{Event, Location};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

pub fn history(path: &Path) -> rusqlite::Result<Connection> {
    super::migrations::open_history(path)
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

/// Owns one migrated history connection; application code uses records, not SQL.
pub struct HistoryStore {
    connection: Connection,
}

pub struct HistoryRecord {
    pub checked_at: String,
    pub remaining_kwh: Option<String>,
    pub supply_status: Option<String>,
    pub error: Option<String>,
    pub location: Option<Location>,
}

impl HistoryStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Ok(Self {
            connection: history(path)?,
        })
    }

    /// Retained for integration with the existing public delivery-engine constructor.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn record(&self, event: &Event) -> rusqlite::Result<()> {
        save_event(&self.connection, event)
    }
    pub fn alert_due(&self, topic: &str, now: i64, repeat_after: u64) -> rusqlite::Result<bool> {
        alert_due(&self.connection, topic, now, repeat_after)
    }
    pub fn mark_alert(&self, topic: &str, now: i64) -> rusqlite::Result<()> {
        mark_alert(&self.connection, topic, now)
    }
    pub fn clear_alert(&self, topic: &str) -> rusqlite::Result<()> {
        clear_alert(&self.connection, topic)
    }

    pub fn recent(&self, limit: u32) -> rusqlite::Result<Vec<HistoryRecord>> {
        let mut statement = self.connection.prepare("SELECT checked_at, remaining_kwh, supply_status, error, campus, building, floor, room FROM readings ORDER BY id DESC LIMIT ?1")?;
        statement
            .query_map([limit], |row| {
                let location = match (
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ) {
                    (Some(campus), Some(building), Some(floor), Some(room)) => Some(Location {
                        campus,
                        building,
                        floor,
                        room,
                    }),
                    _ => None,
                };
                Ok(HistoryRecord {
                    checked_at: row.get(0)?,
                    remaining_kwh: row.get(1)?,
                    supply_status: row.get(2)?,
                    error: row.get(3)?,
                    location,
                })
            })?
            .collect()
    }
}
