//! Durable delivery records and transactions; no notification or retry policy lives here.
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) struct Stored {
    pub payload: String,
    pub attempts: u32,
    pub status: String,
    pub next_attempt: Option<i64>,
    pub round_started: i64,
}

pub(crate) struct Pending<'a> {
    pub event_type: &'a str,
    pub payload: &'a str,
    pub attempts: u32,
    pub next_attempt: i64,
    pub round_started: i64,
    pub now: i64,
}

pub(crate) struct Attempt {
    pub attempts: u32,
    pub next_attempt: Option<i64>,
    pub status: &'static str,
    pub now: i64,
}

pub(crate) struct Latest {
    pub status: String,
    pub attempts: u32,
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_sent_at: Option<i64>,
}

pub(crate) struct DeliveryStore<'a> {
    connection: &'a Connection,
    instance: String,
}

impl<'a> DeliveryStore<'a> {
    pub(crate) fn new(connection: &'a Connection, instance: &str) -> rusqlite::Result<Self> {
        super::migrations::prepare_delivery(connection)?;
        // An explicit restart may follow a credential fix. Retryable budgets remain intact.
        connection.execute(
            "DELETE FROM daemon_delivery WHERE instance=?1 AND status='blocked'",
            [instance],
        )?;
        Ok(Self {
            connection,
            instance: instance.into(),
        })
    }

    fn key(&self, topic: &str, channel: &str) -> String {
        serde_json::to_string(&("daemon:v1", &self.instance, topic, channel))
            .expect("string serialization")
    }

    pub(crate) fn stored(&self, topic: &str, channel: &str) -> rusqlite::Result<Option<Stored>> {
        self.connection.query_row(
            "SELECT payload,attempts,status,next_attempt,round_started FROM daemon_delivery WHERE instance=?1 AND topic=?2 AND channel=?3",
            params![self.instance, topic, channel], |row| Ok(Stored {
                payload: row.get(0)?, attempts: row.get(1)?, status: row.get(2)?,
                next_attempt: row.get(3)?, round_started: row.get(4)?,
            }),
        ).optional()
    }

    pub(crate) fn due(
        &self,
        topic: &str,
        channel: &str,
        now: i64,
        repeat_after: u64,
    ) -> rusqlite::Result<bool> {
        super::history::alert_due(
            self.connection,
            &self.key(topic, channel),
            now,
            repeat_after,
        )
    }

    pub(crate) fn clear_kind(&self, kind: &str) -> rusqlite::Result<()> {
        let entries = {
            let mut statement = self.connection.prepare(
                "SELECT topic,channel FROM daemon_delivery WHERE instance=?1 AND event_type=?2",
            )?;
            statement
                .query_map(params![self.instance, kind], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let transaction = self.connection.unchecked_transaction()?;
        for (topic, channel) in entries {
            super::history::clear_alert(&transaction, &self.key(&topic, &channel))?;
        }
        transaction.execute(
            "DELETE FROM daemon_delivery WHERE instance=?1 AND event_type=?2",
            params![self.instance, kind],
        )?;
        transaction.commit()
    }

    pub(crate) fn cancel_pending(&self) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE daemon_delivery SET status='cancelled' WHERE instance=?1 AND status='pending'",
            [&self.instance],
        )?;
        Ok(())
    }

    pub(crate) fn enqueue(
        &self,
        topic: &str,
        channel: &str,
        record: Pending<'_>,
    ) -> rusqlite::Result<()> {
        self.connection.execute("INSERT INTO daemon_delivery
            (instance,topic,channel,event_type,payload,attempts,status,next_attempt,round_started,updated_at,last_error)
            VALUES (?1,?2,?3,?4,?5,?6,'pending',?7,?8,?9,NULL)
            ON CONFLICT(instance,topic,channel) DO UPDATE SET payload=excluded.payload,
            attempts=excluded.attempts,status='pending',next_attempt=excluded.next_attempt,
            round_started=excluded.round_started,updated_at=excluded.updated_at",
            params![self.instance, topic, channel, record.event_type, record.payload, record.attempts, record.next_attempt, record.round_started, record.now])?;
        Ok(())
    }

    pub(crate) fn record_attempt(
        &self,
        topic: &str,
        channel: &str,
        attempt: Attempt,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE daemon_delivery SET attempts=?4,next_attempt=?5,status=?6,updated_at=?7
            WHERE instance=?1 AND topic=?2 AND channel=?3",
            params![
                self.instance,
                topic,
                channel,
                attempt.attempts,
                attempt.next_attempt,
                attempt.status,
                attempt.now
            ],
        )?;
        Ok(())
    }

    pub(crate) fn accept(&self, topic: &str, channel: &str, now: i64) -> rusqlite::Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        super::history::mark_alert(&transaction, &self.key(topic, channel), now)?;
        transaction.execute(
            "UPDATE daemon_delivery SET status='accepted',next_attempt=NULL,last_error=NULL
            WHERE instance=?1 AND topic=?2 AND channel=?3",
            params![self.instance, topic, channel],
        )?;
        transaction.commit()
    }

    pub(crate) fn fail(
        &self,
        topic: &str,
        channel: &str,
        status: &str,
        next: Option<i64>,
        error: &str,
    ) -> rusqlite::Result<()> {
        self.connection.execute(
            "UPDATE daemon_delivery SET status=?4,next_attempt=?5,last_error=?6
            WHERE instance=?1 AND topic=?2 AND channel=?3",
            params![self.instance, topic, channel, status, next, error],
        )?;
        Ok(())
    }

    pub(crate) fn latest(&self, channel: &str) -> rusqlite::Result<Option<Latest>> {
        let state = self
            .connection
            .query_row(
                "SELECT topic,status,attempts,next_attempt,last_error FROM daemon_delivery
            WHERE instance=?1 AND channel=?2 ORDER BY updated_at DESC,rowid DESC LIMIT 1",
                params![self.instance, channel],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        Latest {
                            status: row.get(1)?,
                            attempts: row.get(2)?,
                            next_attempt_at: row.get(3)?,
                            last_error: row.get(4)?,
                            last_sent_at: None,
                        },
                    ))
                },
            )
            .optional()?;
        match state {
            Some((topic, mut state)) => {
                state.last_sent_at = self
                    .connection
                    .query_row(
                        "SELECT last_sent FROM alert_state WHERE topic=?1",
                        [self.key(&topic, channel)],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }
}
