use rust_decimal::Decimal;
use sdu_infohelper::{
    Event, Location, QueryError, Reading, history,
    monitor::{
        Comparison,
        delivery::{Channel, DeliveryEngine},
    },
    notification::{Alert, DeliveryReceipt, Notifier, NotifyError},
};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, str::FromStr};

#[derive(Clone)]
struct Fake {
    results: Rc<RefCell<VecDeque<bool>>>,
    sent: Rc<RefCell<Vec<Alert>>>,
    permanent: bool,
}
impl Fake {
    fn new(results: &[bool]) -> Self {
        Self {
            results: Rc::new(RefCell::new(results.iter().copied().collect())),
            sent: Rc::new(RefCell::new(Vec::new())),
            permanent: false,
        }
    }
    fn channel(&self, id: &str) -> Channel {
        Channel {
            id: id.into(),
            notifier: Box::new(self.clone()),
        }
    }
    fn count(&self) -> usize {
        self.sent.borrow().len()
    }
}
impl Notifier for Fake {
    fn send(&self, alert: &Alert) -> Result<DeliveryReceipt, NotifyError> {
        self.sent.borrow_mut().push(alert.clone());
        if self.results.borrow_mut().pop_front().unwrap_or(true) {
            Ok(DeliveryReceipt)
        } else {
            Err(NotifyError {
                message: "模拟通知失败。".into(),
                retryable: !self.permanent,
                retry_after_seconds: None,
            })
        }
    }
}

fn reading(balance: &str, threshold: &str, room: &str) -> Event {
    let remaining = Decimal::from_str(balance).unwrap();
    let threshold = Decimal::from_str(threshold).unwrap();
    let mut event = Event::success(
        Reading {
            remaining_kwh: remaining,
            supply_status: None,
        },
        Some(threshold),
        None,
    )
    .at_location(Location {
        campus: "C&校区".into(),
        building: "B&楼栋".into(),
        floor: "F&楼层".into(),
        room: room.into(),
    });
    event.low_balance = Some(Comparison::StrictlyBelow.low(remaining, threshold));
    event
}

#[test]
fn channels_cool_down_independently_and_round_robin_without_waiting_for_retries() {
    let dir = tempfile::tempdir().unwrap();
    let conn = history(&dir.path().join("history.sqlite3")).unwrap();
    let fail = Fake::new(&[false, true]);
    let good = Fake::new(&[true]);
    let mut engine = DeliveryEngine::new(
        &conn,
        "instance",
        vec![fail.channel("p"), good.channel("w")],
        120,
        60,
    )
    .unwrap();
    engine
        .observe(&reading("9", "10", "R1"), false, 100)
        .unwrap();
    assert!(!engine.dispatch_next(100).unwrap().unwrap().accepted);
    assert!(engine.dispatch_next(100).unwrap().unwrap().accepted);
    assert!(engine.dispatch_next(104).unwrap().is_none());
    assert!(engine.dispatch_next(105).unwrap().unwrap().accepted);
    let good_alerts = good.sent.borrow();
    let event_id = &good_alerts[0].event_id;
    assert!(
        fail.sent
            .borrow()
            .iter()
            .all(|alert| &alert.event_id == event_id)
    );
    drop(good_alerts);
    engine
        .observe(&reading("8", "10.00", "R1"), false, 160)
        .unwrap();
    assert!(engine.dispatch_next(160).unwrap().is_none());
    assert_eq!(good.count(), 1);
    engine
        .observe(&reading("7", "10", "R1"), false, 220)
        .unwrap();
    assert_eq!(engine.dispatch_next(220).unwrap().unwrap().channel, "w");
    assert!(engine.dispatch_next(220).unwrap().is_none());
    engine
        .observe(&reading("7", "10", "R1"), false, 225)
        .unwrap();
    assert_eq!(engine.dispatch_next(225).unwrap().unwrap().channel, "p");
}

#[test]
fn recovery_and_target_changes_cancel_stale_messages_and_rearm_alerts() {
    let dir = tempfile::tempdir().unwrap();
    let conn = history(&dir.path().join("history.sqlite3")).unwrap();
    let sender = Fake::new(&[false, true, true]);
    let mut engine = DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
    engine
        .observe(&reading("9", "10", "R1"), false, 100)
        .unwrap();
    engine.dispatch_next(100).unwrap();
    engine
        .observe(&reading("10", "10", "R1"), false, 102)
        .unwrap();
    assert!(engine.dispatch_next(105).unwrap().is_none());
    engine
        .observe(&reading("9", "10", "R1"), false, 106)
        .unwrap();
    assert!(engine.dispatch_next(106).unwrap().unwrap().accepted);
    assert_ne!(
        sender.sent.borrow()[0].event_id,
        sender.sent.borrow()[1].event_id
    );
    engine
        .observe(&reading("1", "10", "R2"), false, 107)
        .unwrap();
    assert!(engine.dispatch_next(107).unwrap().unwrap().accepted);
    assert_eq!(
        sender.sent.borrow()[2].location.as_ref().unwrap().room,
        "R2"
    );
}

#[test]
fn failures_cancel_low_alerts_and_authentication_has_its_own_cooldown() {
    let dir = tempfile::tempdir().unwrap();
    let conn = history(&dir.path().join("history.sqlite3")).unwrap();
    let sender = Fake::new(&[false, true, true]);
    let mut engine = DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
    engine
        .observe(&reading("1", "10", "R"), false, 100)
        .unwrap();
    engine.dispatch_next(100).unwrap();
    engine
        .observe(&Event::failure(&QueryError::Network), false, 102)
        .unwrap();
    assert!(engine.dispatch_next(106).unwrap().is_none());
    engine
        .observe(&Event::failure(&QueryError::Authentication), true, 110)
        .unwrap();
    engine.dispatch_next(110).unwrap();
    assert_eq!(sender.sent.borrow()[1].event, "electricity.auth_required");
    assert!(sender.sent.borrow()[1].remaining_kwh.is_none());
    engine
        .observe(&Event::failure(&QueryError::Authentication), true, 170)
        .unwrap();
    assert!(engine.dispatch_next(170).unwrap().is_none());
    engine
        .observe(&reading("20", "10", "R"), false, 180)
        .unwrap();
    engine
        .observe(&Event::failure(&QueryError::Authentication), true, 181)
        .unwrap();
    assert!(engine.dispatch_next(181).unwrap().unwrap().accepted);
}

#[test]
fn budgets_cooldowns_and_retry_deadlines_survive_restart_and_fresh_query_is_required() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.sqlite3");
    let sender = Fake::new(&[false, false, false, true]);
    {
        let conn = history(&path).unwrap();
        let mut engine =
            DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
        engine
            .observe(&reading("9", "10", "R"), false, 100)
            .unwrap();
        engine.dispatch_next(100).unwrap();
    }
    {
        let conn = history(&path).unwrap();
        let mut engine =
            DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
        assert!(engine.dispatch_next(105).unwrap().is_none());
        engine
            .observe(&reading("8", "10", "R"), false, 102)
            .unwrap();
        assert!(engine.dispatch_next(102).unwrap().is_none());
        engine.dispatch_next(105).unwrap();
        engine.dispatch_next(135).unwrap();
        assert_eq!(sender.count(), 3);
        assert!(engine.dispatch_next(136).unwrap().is_none());
    }
    let conn = history(&path).unwrap();
    let mut engine = DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
    engine
        .observe(&reading("7", "10", "R"), false, 140)
        .unwrap();
    assert!(engine.dispatch_next(140).unwrap().is_none());
    engine
        .observe(&reading("6", "10", "R"), false, 160)
        .unwrap();
    assert!(engine.dispatch_next(160).unwrap().unwrap().accepted);
    assert_eq!(sender.count(), 4);
    assert_eq!(
        sender.sent.borrow()[0].event_id,
        sender.sent.borrow()[2].event_id
    );
    assert_ne!(
        sender.sent.borrow()[2].event_id,
        sender.sent.borrow()[3].event_id
    );
    drop(engine);
    let mut engine = DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
    engine
        .observe(&reading("5", "10", "R"), false, 161)
        .unwrap();
    assert!(engine.dispatch_next(161).unwrap().is_none());
    assert_eq!(engine.states().unwrap()[0].last_sent_at, Some(160));
}

#[test]
fn permanent_errors_wait_for_restart_and_instances_do_not_share_state() {
    let dir = tempfile::tempdir().unwrap();
    let conn = history(&dir.path().join("history.sqlite3")).unwrap();
    let mut sender = Fake::new(&[false, true, true]);
    sender.permanent = true;
    let mut engine = DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
    engine
        .observe(&reading("1", "10", "R"), false, 100)
        .unwrap();
    engine.dispatch_next(100).unwrap();
    engine
        .observe(&reading("1", "10", "R"), false, 200)
        .unwrap();
    assert!(engine.dispatch_next(200).unwrap().is_none());
    assert_eq!(engine.states().unwrap()[0].status, "blocked");
    let mut other =
        DeliveryEngine::new(&conn, "other", vec![sender.channel("w")], 120, 60).unwrap();
    other.observe(&reading("1", "10", "R"), false, 201).unwrap();
    assert!(other.dispatch_next(201).unwrap().unwrap().accepted);
    drop(engine);
    let mut engine = DeliveryEngine::new(&conn, "i", vec![sender.channel("w")], 120, 60).unwrap();
    engine
        .observe(&reading("1", "10", "R"), false, 202)
        .unwrap();
    assert!(engine.dispatch_next(202).unwrap().unwrap().accepted);
}
