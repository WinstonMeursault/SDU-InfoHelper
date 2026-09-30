//! Durable per-channel cooldown and retry budgets. No network call holds a DB transaction.
use crate::{
    Event, alert_due, clear_alert, mark_alert,
    notification::{Alert, HttpNotifier, Notifier},
    settings::NotificationSettings,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, String>;
const LOW: &str = "electricity.low_balance";
const AUTH: &str = "electricity.auth_required";

pub struct Channel {
    pub id: String,
    pub notifier: Box<dyn Notifier>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ChannelState {
    pub id: String,
    pub status: String,
    pub attempts: u32,
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub last_sent_at: Option<i64>,
}

pub struct DeliveryReport {
    pub channel: String,
    pub accepted: bool,
    pub error: Option<String>,
}

struct Stored {
    payload: String,
    attempts: u32,
    status: String,
    next_attempt: Option<i64>,
    round_started: i64,
}

pub struct DeliveryEngine<'a> {
    connection: &'a Connection,
    instance: String,
    channels: Vec<Channel>,
    repeat_after: u64,
    interval: u64,
    active_topic: Option<String>,
    cursor: usize,
}

fn db<T>(result: rusqlite::Result<T>) -> Result<T> {
    result.map_err(|_| "无法读写通知状态数据库。".into())
}

pub fn channels(settings: &NotificationSettings) -> Result<Vec<Channel>> {
    settings.require_enabled().map_err(|e| e.to_string())?;
    settings
        .channels
        .iter()
        .filter(|c| c.enabled())
        .map(|config| {
            Ok(Channel {
                id: config.id().into(),
                notifier: Box::new(HttpNotifier::new(config).map_err(|e| e.to_string())?),
            })
        })
        .collect()
}

impl<'a> DeliveryEngine<'a> {
    pub fn new(
        connection: &'a Connection,
        instance: &str,
        channels: Vec<Channel>,
        repeat_after: u64,
        interval: u64,
    ) -> Result<Self> {
        db(connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS daemon_delivery (
            instance TEXT NOT NULL, topic TEXT NOT NULL, channel TEXT NOT NULL,
            event_type TEXT NOT NULL, payload TEXT NOT NULL, attempts INTEGER NOT NULL,
            status TEXT NOT NULL, next_attempt INTEGER, round_started INTEGER NOT NULL,
            updated_at INTEGER NOT NULL, last_error TEXT,
            PRIMARY KEY (instance, topic, channel)
        );",
        ))?;
        // An explicit restart may follow a credential fix. Retryable budgets remain intact.
        db(connection.execute(
            "DELETE FROM daemon_delivery WHERE instance=?1 AND status='blocked'",
            [instance],
        ))?;
        Ok(Self {
            connection,
            instance: instance.into(),
            channels,
            repeat_after,
            interval,
            active_topic: None,
            cursor: 0,
        })
    }

    fn key(&self, topic: &str, channel: &str) -> String {
        serde_json::to_string(&("daemon:v1", &self.instance, topic, channel))
            .expect("string serialization")
    }

    fn topic(alert: &Alert) -> String {
        let location = if alert.event == LOW {
            alert.location.as_ref()
        } else {
            None
        };
        let threshold = alert
            .threshold_kwh
            .as_deref()
            .and_then(|value| value.parse::<rust_decimal::Decimal>().ok())
            .map(|value| value.normalize().to_string());
        serde_json::to_string(&(&alert.event, location, threshold))
            .expect("alert topic serialization")
    }

    fn stored(&self, topic: &str, channel: &str) -> Result<Option<Stored>> {
        db(self.connection.query_row(
            "SELECT payload,attempts,status,next_attempt,round_started FROM daemon_delivery WHERE instance=?1 AND topic=?2 AND channel=?3",
            params![self.instance, topic, channel], |row| Ok(Stored {
                payload: row.get(0)?, attempts: row.get(1)?, status: row.get(2)?,
                next_attempt: row.get(3)?, round_started: row.get(4)?,
            }),
        ).optional())
    }

    fn clear_kind(&self, kind: &str) -> Result<()> {
        let mut statement = db(self.connection.prepare(
            "SELECT topic,channel FROM daemon_delivery WHERE instance=?1 AND event_type=?2",
        ))?;
        let entries = db(db(statement.query_map(params![self.instance, kind], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }))?
        .collect::<rusqlite::Result<Vec<_>>>())?;
        let transaction = db(self.connection.unchecked_transaction())?;
        for (topic, channel) in entries {
            db(clear_alert(&transaction, &self.key(&topic, &channel)))?;
        }
        db(transaction.execute(
            "DELETE FROM daemon_delivery WHERE instance=?1 AND event_type=?2",
            params![self.instance, kind],
        ))?;
        db(transaction.commit())
    }

    fn cancel_pending(&self) -> Result<()> {
        db(self.connection.execute(
            "UPDATE daemon_delivery SET status='cancelled' WHERE instance=?1 AND status='pending'",
            [&self.instance],
        ))?;
        Ok(())
    }

    /// Called after every query, including failures, before any retry is dispatched.
    pub fn observe(&mut self, event: &Event, needs_login: bool, now: i64) -> Result<()> {
        self.active_topic = None;
        if event.error.is_none() {
            self.clear_kind(AUTH)?;
            if event.low_balance == Some(true) {
                self.enqueue(&Alert::low_balance(event), now)?;
            } else if event.low_balance == Some(false) {
                self.clear_kind(LOW)?;
            }
        } else {
            self.cancel_pending()?;
            if needs_login {
                self.enqueue(&Alert::auth_required(event.location.clone()), now)?;
            }
        }
        Ok(())
    }

    fn enqueue(&mut self, alert: &Alert, now: i64) -> Result<()> {
        let topic = Self::topic(alert);
        self.active_topic = Some(topic.clone());
        for channel in &self.channels {
            if !db(alert_due(
                self.connection,
                &self.key(&topic, &channel.id),
                now,
                self.repeat_after,
            ))? {
                continue;
            }
            let previous = self.stored(&topic, &channel.id)?;
            let mut payload = alert.clone();
            let (attempts, next_attempt, round_started) = match previous {
                Some(previous) if previous.status == "blocked" => continue,
                Some(previous)
                    if previous.status != "accepted"
                        && (previous.attempts < 3
                            || now.saturating_sub(previous.round_started)
                                < self.interval as i64) =>
                {
                    if previous.attempts >= 3 {
                        continue;
                    }
                    let old: Alert = serde_json::from_str(&previous.payload)
                        .map_err(|_| "持久化通知事件格式无效。")?;
                    payload.event_id = old.event_id;
                    (
                        previous.attempts,
                        previous.next_attempt.unwrap_or(now),
                        previous.round_started,
                    )
                }
                _ => (0, now, now),
            };
            let serialized = serde_json::to_string(&payload).map_err(|_| "无法序列化通知事件。")?;
            db(self.connection.execute("INSERT INTO daemon_delivery
                (instance,topic,channel,event_type,payload,attempts,status,next_attempt,round_started,updated_at,last_error)
                VALUES (?1,?2,?3,?4,?5,?6,'pending',?7,?8,?9,NULL)
                ON CONFLICT(instance,topic,channel) DO UPDATE SET payload=excluded.payload,
                attempts=excluded.attempts,status='pending',next_attempt=excluded.next_attempt,
                round_started=excluded.round_started,updated_at=excluded.updated_at",
                params![self.instance, topic, channel.id, alert.event, serialized, attempts, next_attempt, round_started, now]))?;
        }
        Ok(())
    }

    /// Send at most one message. The caller checks stop and query deadlines between calls.
    pub fn dispatch_next(&mut self, now: i64) -> Result<Option<DeliveryReport>> {
        let Some(topic) = self.active_topic.clone() else {
            return Ok(None);
        };
        for offset in 0..self.channels.len() {
            let index = (self.cursor + offset) % self.channels.len();
            let channel = &self.channels[index];
            let Some(stored) = self.stored(&topic, &channel.id)? else {
                continue;
            };
            if stored.status != "pending"
                || stored.attempts >= 3
                || stored.next_attempt.is_none_or(|time| time > now)
            {
                continue;
            }
            self.cursor = (index + 1) % self.channels.len();
            let alert: Alert =
                serde_json::from_str(&stored.payload).map_err(|_| "持久化通知事件格式无效。")?;
            let attempts = stored.attempts + 1;
            let delay = if attempts == 1 { 5 } else { 30 };
            let next = (attempts < 3).then(|| now.saturating_add(delay));
            // Consume budget before sending: a crash after remote acceptance cannot reset it.
            db(self.connection.execute(
                "UPDATE daemon_delivery SET attempts=?4,next_attempt=?5,status=?6,updated_at=?7
                WHERE instance=?1 AND topic=?2 AND channel=?3",
                params![
                    self.instance,
                    topic,
                    channel.id,
                    attempts,
                    next,
                    if attempts < 3 { "pending" } else { "exhausted" },
                    now
                ],
            ))?;
            let result = channel.notifier.send(&alert);
            let report = match result {
                Ok(_) => {
                    let transaction = db(self.connection.unchecked_transaction())?;
                    db(mark_alert(
                        &transaction,
                        &self.key(&topic, &channel.id),
                        now,
                    ))?;
                    db(transaction.execute("UPDATE daemon_delivery SET status='accepted',next_attempt=NULL,last_error=NULL
                        WHERE instance=?1 AND topic=?2 AND channel=?3", params![self.instance, topic, channel.id]))?;
                    db(transaction.commit())?;
                    DeliveryReport {
                        channel: channel.id.clone(),
                        accepted: true,
                        error: None,
                    }
                }
                Err(error) => {
                    let status = if !error.retryable {
                        "blocked"
                    } else if attempts >= 3 {
                        "exhausted"
                    } else {
                        "pending"
                    };
                    let next = (status == "pending").then(|| {
                        now.saturating_add(
                            error
                                .retry_after_seconds
                                .unwrap_or(delay as u64)
                                .min(self.interval)
                                .max(1) as i64,
                        )
                    });
                    db(self.connection.execute(
                        "UPDATE daemon_delivery SET status=?4,next_attempt=?5,last_error=?6
                        WHERE instance=?1 AND topic=?2 AND channel=?3",
                        params![
                            self.instance,
                            topic,
                            channel.id,
                            status,
                            next,
                            error.message
                        ],
                    ))?;
                    DeliveryReport {
                        channel: channel.id.clone(),
                        accepted: false,
                        error: Some(error.message),
                    }
                }
            };
            return Ok(Some(report));
        }
        Ok(None)
    }

    pub fn states(&self) -> Result<Vec<ChannelState>> {
        self.channels
            .iter()
            .map(|channel| {
                let state = db(self
                    .connection
                    .query_row(
                        "SELECT topic,status,attempts,next_attempt,last_error FROM daemon_delivery
                WHERE instance=?1 AND channel=?2 ORDER BY updated_at DESC,rowid DESC LIMIT 1",
                        params![self.instance, channel.id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                ChannelState {
                                    id: channel.id.clone(),
                                    status: row.get(1)?,
                                    attempts: row.get(2)?,
                                    next_attempt_at: row.get(3)?,
                                    last_error: row.get(4)?,
                                    last_sent_at: None,
                                },
                            ))
                        },
                    )
                    .optional())?;
                match state {
                    Some((topic, mut state)) => {
                        state.last_sent_at = db(self
                            .connection
                            .query_row(
                                "SELECT last_sent FROM alert_state WHERE topic=?1",
                                [self.key(&topic, &channel.id)],
                                |row| row.get(0),
                            )
                            .optional())?;
                        Ok(state)
                    }
                    None => Ok(ChannelState {
                        id: channel.id.clone(),
                        status: "idle".into(),
                        attempts: 0,
                        next_attempt_at: None,
                        last_error: None,
                        last_sent_at: None,
                    }),
                }
            })
            .collect()
    }
}
