//! Durable per-channel cooldown and retry budgets. No network call holds a DB transaction.
use crate::{
    Event,
    domain::DeliveryStatus,
    monitor::observation::{FailureDisposition, Observation, Outcome},
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

enum Notice {
    LowBalance(Alert, String),
    Recovered,
    NoThreshold,
    AuthRequired(Alert, String),
    Failed,
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

    /// Compatibility adapter for callers that already supply the stable Event DTO.
    pub fn observe(&mut self, event: &Event, needs_login: bool, now: i64) -> Result<()> {
        let notice = if event.error.is_none() {
            match event.low_balance {
                Some(true) => {
                    let alert = Alert::low_balance(event);
                    let topic = Self::topic(&alert);
                    Notice::LowBalance(alert, topic)
                }
                Some(false) => Notice::Recovered,
                None => Notice::NoThreshold,
            }
        } else if needs_login {
            let alert = Alert::auth_required(event.location.clone());
            let topic = Self::topic(&alert);
            Notice::AuthRequired(alert, topic)
        } else {
            Notice::Failed
        };
        self.observe_notice(notice, now)
    }

    pub(crate) fn observe_observation(
        &mut self,
        observation: &Observation,
        now: i64,
    ) -> Result<()> {
        let notice = match &observation.outcome {
            Outcome::Success(success) => match success.low_balance() {
                Some(true) => {
                    let alert = Alert::low_balance(&Event::from(observation));
                    let topic = serde_json::to_string(&(
                        LOW,
                        Some(&success.location),
                        success.threshold.map(|value| value.normalize().to_string()),
                    ))
                    .expect("alert topic serialization");
                    Notice::LowBalance(alert, topic)
                }
                Some(false) => Notice::Recovered,
                None => Notice::NoThreshold,
            },
            Outcome::Failure {
                location,
                disposition: FailureDisposition::NeedsLogin,
                ..
            } => {
                let alert = Alert::auth_required(location.clone());
                let topic = Self::topic(&alert);
                Notice::AuthRequired(alert, topic)
            }
            Outcome::Failure { .. } => Notice::Failed,
        };
        self.observe_notice(notice, now)
    }

    fn observe_notice(&mut self, notice: Notice, now: i64) -> Result<()> {
        self.active_topic = None;
        match notice {
            Notice::LowBalance(alert, topic) => {
                db(self.store.clear_kind(AUTH))?;
                self.enqueue(topic, &alert, now)?;
            }
            Notice::Recovered => {
                db(self.store.clear_kind(AUTH))?;
                db(self.store.clear_kind(LOW))?;
            }
            Notice::NoThreshold => {
                db(self.store.clear_kind(AUTH))?;
            }
            Notice::AuthRequired(alert, topic) => {
                db(self.store.cancel_pending())?;
                self.enqueue(topic, &alert, now)?;
            }
            Notice::Failed => {
                db(self.store.cancel_pending())?;
            }
        }
        Ok(())
    }

    fn enqueue(&mut self, topic: String, alert: &Alert, now: i64) -> Result<()> {
        self.active_topic = Some(topic.clone());
        for channel in &self.channels {
            if !db(self.store.due(&topic, &channel.id, now, self.repeat_after))? {
                continue;
            }
            let previous = db(self.store.stored(&topic, &channel.id))?;
            let mut payload = alert.clone();
            let (attempts, next_attempt, round_started) = match previous {
                Some(previous) if previous.status == DeliveryStatus::Blocked => continue,
                Some(previous)
                    if previous.status != DeliveryStatus::Accepted
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
            if stored.status != DeliveryStatus::Pending
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
                    status: if attempts < 3 {
                        DeliveryStatus::Pending
                    } else {
                        DeliveryStatus::Exhausted
                    },
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
                        DeliveryStatus::Blocked
                    } else if attempts >= 3 {
                        DeliveryStatus::Exhausted
                    } else {
                        DeliveryStatus::Pending
                    };
                    let next = (status == DeliveryStatus::Pending).then(|| {
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
                        status: state.status.as_str().into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Location, QueryError, Reading,
        monitor::Comparison,
        notification::{DeliveryReceipt, NotifyError},
    };
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone)]
    struct Fake(Rc<RefCell<Vec<Alert>>>);

    impl Notifier for Fake {
        fn send(&self, alert: &Alert) -> std::result::Result<DeliveryReceipt, NotifyError> {
            let mut sent = self.0.borrow_mut();
            sent.push(alert.clone());
            if sent.len() == 1 {
                Err(NotifyError {
                    message: "暂时失败".into(),
                    retryable: true,
                    retry_after_seconds: None,
                })
            } else {
                Ok(DeliveryReceipt)
            }
        }
    }

    fn reading(value: &str) -> Observation {
        Observation::success(
            Reading {
                remaining_kwh: value.parse().unwrap(),
                supply_status: None,
            },
            Location {
                campus: "C".into(),
                building: "B".into(),
                floor: "F".into(),
                room: "R".into(),
            },
            Some("10.00".parse().unwrap()),
            Comparison::StrictlyBelow,
            None,
        )
    }

    #[test]
    fn typed_and_legacy_event_paths_produce_equivalent_cooldowns_retries_and_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let typed_history = HistoryStore::open(&dir.path().join("typed.sqlite3")).unwrap();
        let legacy_history = HistoryStore::open(&dir.path().join("legacy.sqlite3")).unwrap();
        let typed_sender = Fake(Rc::new(RefCell::new(Vec::new())));
        let legacy_sender = Fake(Rc::new(RefCell::new(Vec::new())));
        let mut typed = DeliveryEngine::for_history(
            &typed_history,
            "instance",
            vec![Channel {
                id: "test".into(),
                notifier: Box::new(typed_sender.clone()),
            }],
            120,
            60,
        )
        .unwrap();
        let mut legacy = DeliveryEngine::for_history(
            &legacy_history,
            "instance",
            vec![Channel {
                id: "test".into(),
                notifier: Box::new(legacy_sender.clone()),
            }],
            120,
            60,
        )
        .unwrap();
        for (now, observation) in [
            (100, reading("9.50")),
            (102, reading("8.25")),
            (105, reading("7.000")),
            (106, Observation::failure(QueryError::Network, None)),
            (107, Observation::failure(QueryError::Authentication, None)),
            (108, reading("10.00")),
            (109, reading("1.00")),
        ] {
            typed.observe_observation(&observation, now).unwrap();
            legacy
                .observe(&Event::from(&observation), observation.flags().1, now)
                .unwrap();
            let typed_report = typed
                .dispatch_next(now)
                .unwrap()
                .map(|report| (report.channel, report.accepted, report.error));
            let legacy_report = legacy
                .dispatch_next(now)
                .unwrap()
                .map(|report| (report.channel, report.accepted, report.error));
            assert_eq!(typed_report, legacy_report);
            assert_eq!(
                serde_json::to_value(typed.states().unwrap()).unwrap(),
                serde_json::to_value(legacy.states().unwrap()).unwrap()
            );
        }
        for sender in [&typed_sender, &legacy_sender] {
            let sent = sender.0.borrow();
            assert_eq!(sent.len(), 4);
            assert_eq!(
                sent[0].event_id, sent[1].event_id,
                "pending event identity must survive a fresh query"
            );
            assert_eq!(sent[2].event, AUTH);
            assert!(sent[2].remaining_kwh.is_none());
            assert_ne!(
                sent[1].event_id, sent[3].event_id,
                "recovery rearms a new logical event"
            );
        }
        let payloads = |sender: &Fake| {
            sender
                .0
                .borrow()
                .iter()
                .map(|alert| {
                    let mut payload = serde_json::to_value(alert).unwrap();
                    payload.as_object_mut().unwrap().remove("event_id");
                    payload.as_object_mut().unwrap().remove("checked_at");
                    payload
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(payloads(&typed_sender), payloads(&legacy_sender));
    }
}
