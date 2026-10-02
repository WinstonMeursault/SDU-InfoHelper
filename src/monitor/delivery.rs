//! Durable per-channel cooldown and retry budgets. No network call holds a DB transaction.
use crate::{
    Event,
    notification::{Alert, HttpNotifier, Notifier},
    settings::NotificationSettings,
    storage::{
        HistoryStore,
        delivery::{Attempt, DeliveryStore, Pending},
    },
};
use rusqlite::Connection;
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

pub struct DeliveryEngine<'a> {
    store: DeliveryStore<'a>,
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
        let store = db(DeliveryStore::new(connection, instance))?;
        Ok(Self {
            store,
            channels,
            repeat_after,
            interval,
            active_topic: None,
            cursor: 0,
        })
    }

    pub fn for_history(
        history: &'a HistoryStore,
        instance: &str,
        channels: Vec<Channel>,
        repeat_after: u64,
        interval: u64,
    ) -> Result<Self> {
        Self::new(
            history.connection(),
            instance,
            channels,
            repeat_after,
            interval,
        )
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

    /// Called after every query, including failures, before any retry is dispatched.
    pub fn observe(&mut self, event: &Event, needs_login: bool, now: i64) -> Result<()> {
        self.active_topic = None;
        if event.error.is_none() {
            db(self.store.clear_kind(AUTH))?;
            if event.low_balance == Some(true) {
                self.enqueue(&Alert::low_balance(event), now)?;
            } else if event.low_balance == Some(false) {
                db(self.store.clear_kind(LOW))?;
            }
        } else {
            db(self.store.cancel_pending())?;
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
            if !db(self.store.due(&topic, &channel.id, now, self.repeat_after))? {
                continue;
            }
            let previous = db(self.store.stored(&topic, &channel.id))?;
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
            db(self.store.enqueue(
                &topic,
                &channel.id,
                Pending {
                    event_type: &alert.event,
                    payload: &serialized,
                    attempts,
                    next_attempt,
                    round_started,
                    now,
                },
            ))?;
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
            let Some(stored) = db(self.store.stored(&topic, &channel.id))? else {
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
            db(self.store.record_attempt(
                &topic,
                &channel.id,
                Attempt {
                    attempts,
                    next_attempt: next,
                    status: if attempts < 3 { "pending" } else { "exhausted" },
                    now,
                },
            ))?;
            let result = channel.notifier.send(&alert);
            let report = match result {
                Ok(_) => {
                    db(self.store.accept(&topic, &channel.id, now))?;
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
                    db(self
                        .store
                        .fail(&topic, &channel.id, status, next, &error.message))?;
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
                Ok(match db(self.store.latest(&channel.id))? {
                    Some(state) => ChannelState {
                        id: channel.id.clone(),
                        status: state.status,
                        attempts: state.attempts,
                        next_attempt_at: state.next_attempt_at,
                        last_error: state.last_error,
                        last_sent_at: state.last_sent_at,
                    },
                    None => ChannelState {
                        id: channel.id.clone(),
                        status: "idle".into(),
                        attempts: 0,
                        next_attempt_at: None,
                        last_error: None,
                        last_sent_at: None,
                    },
                })
            })
            .collect()
    }
}
