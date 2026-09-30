use rusqlite::Connection;
use sdu_infohelper::{
    Location, QueryError, Reading, auth,
    service::{
        Actor, Delivery, ElectricityBackend, NotificationSink, PreferencePatch, SchoolReading,
        SendError, Service, ServiceError, UserRequest, napcat,
    },
    settings::Settings,
};
use serde_json::json;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier, Mutex},
    time::Duration,
};

struct Fixture {
    temp: tempfile::TempDir,
    service: Service,
    alice: Actor,
    bob: Actor,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let service = Service::open(&temp.path().join("service"), Duration::from_secs(1)).unwrap();
        let fixture = Self {
            temp,
            service,
            alice: Actor::qq("90001", "10001").unwrap(),
            bob: Actor::qq("90001", "10002").unwrap(),
        };
        for (actor, name) in [(&fixture.alice, "alice"), (&fixture.bob, "bob")] {
            let source = fixture.source(name);
            assert_eq!(
                fixture.service.provision(actor, &source).unwrap().state,
                "pending"
            );
        }
        fixture
    }
    fn source(&self, name: &str) -> PathBuf {
        let source = self.temp.path().join(format!("{name}.yaml"));
        auth::secure_write(&source, format!("cas:\n  username: {name}\n  password: secret-{name}\n  device_id: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nauth:\n  cache: /tmp/shared-auth.json\ndorm_electricity:\n  campus: C&Campus\n  building: B&Building\n  floor: F&Floor\n  room: R&Room\n").as_bytes()).unwrap();
        source
    }
    fn user_dir(&self, actor: &Actor) -> PathBuf {
        let db = Connection::open(self.temp.path().join("service/registry.sqlite3")).unwrap();
        let id: String = db
            .query_row(
                "SELECT id FROM bindings WHERE bot_id=?1 AND user_id=?2",
                [actor.bot_id(), actor.user_id()],
                |row| row.get(0),
            )
            .unwrap();
        self.temp.path().join("service/users").join(id)
    }
}
#[derive(Clone)]
enum Outcome {
    Energy(&'static str),
    Auth,
    Network,
}
struct Backend {
    outcomes: Mutex<HashMap<String, Outcome>>,
    calls: Mutex<Vec<String>>,
}
impl Backend {
    fn new() -> Self {
        Self {
            outcomes: Mutex::new(HashMap::from([
                ("alice".into(), Outcome::Energy("7.01")),
                ("bob".into(), Outcome::Energy("35.67")),
            ])),
            calls: Mutex::new(Vec::new()),
        }
    }
    fn set(&self, user: &str, outcome: Outcome) {
        self.outcomes.lock().unwrap().insert(user.into(), outcome);
    }
}
impl ElectricityBackend for Backend {
    fn read(&self, path: &Path, _: Duration) -> Result<SchoolReading, QueryError> {
        let settings = Settings::load(path)?;
        let account = settings.cas.unwrap().username;
        self.calls.lock().unwrap().push(account.clone());
        match self.outcomes.lock().unwrap().get(&account).unwrap().clone() {
            Outcome::Auth => Err(QueryError::Authentication),
            Outcome::Network => Err(QueryError::Network),
            Outcome::Energy(value) => Ok(SchoolReading {
                reading: Reading {
                    remaining_kwh: value.parse().unwrap(),
                    supply_status: None,
                },
                location: Location {
                    campus: "C&Campus".into(),
                    building: "B&Building".into(),
                    floor: "F&Floor".into(),
                    room: format!("R&{account}"),
                },
            }),
        }
    }
}
fn enable_interval(service: &Service, actor: &Actor) {
    service
        .preferences(
            actor,
            PreferencePatch {
                interval_seconds: Some(300),
                ..Default::default()
            },
        )
        .unwrap();
}

#[test]
fn credentials_device_ids_tokens_history_and_settings_are_isolated() {
    let f = Fixture::new();
    let backend = Backend::new();
    let a_dir = f.user_dir(&f.alice);
    let b_dir = f.user_dir(&f.bob);
    assert_ne!(a_dir, b_dir);
    let a = Settings::load(&a_dir.join("config.yaml")).unwrap();
    let b = Settings::load(&b_dir.join("config.yaml")).unwrap();
    assert_eq!(
        a.cache_path(&a_dir.join("config.yaml")),
        a_dir.join("auth.json")
    );
    assert_ne!(
        a.cas.as_ref().unwrap().device_id,
        b.cas.as_ref().unwrap().device_id
    );
    assert_ne!(
        a.cas.as_ref().unwrap().device_id.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    for (actor, token) in [(&f.alice, "alice-token"), (&f.bob, "bob-token")] {
        let input = f.temp.path().join("oauth.json");
        auth::secure_write(
            &input,
            serde_json::to_string(
                &json!({"access_token":token,"refresh_token":format!("{token}-refresh")}),
            )
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
        f.service
            .import_auth(actor, &input, auth::OAuthProvider::Berserker, None)
            .unwrap();
        let bytes = fs::read(f.user_dir(actor).join("auth.json")).unwrap();
        let cached: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(cached["access_token"], token);
        assert_eq!(
            cached["cas_username"],
            if actor == &f.alice { "alice" } else { "bob" }
        );
    }
    f.service
        .preferences(
            &f.alice,
            PreferencePatch {
                threshold_kwh: Some("5.00".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        f.service.status(&f.bob).unwrap().preferences.threshold_kwh,
        "10"
    );
    f.service.query_with(&f.alice, &backend, 1000).unwrap();
    f.service.query_with(&f.bob, &backend, 1000).unwrap();
    assert_eq!(
        f.service.history(&f.alice, 20).unwrap()[0]
            .remaining_kwh
            .as_deref(),
        Some("7.01")
    );
    assert_eq!(
        f.service.history(&f.bob, 20).unwrap()[0]
            .remaining_kwh
            .as_deref(),
        Some("35.67")
    );
    let public = serde_json::to_string(&f.service.status(&f.alice).unwrap()).unwrap();
    for secret in ["secret-alice", "alice-token", "device_id", "auth.json"] {
        assert!(!public.contains(secret));
    }
    assert!(matches!(
        f.service.provision(&f.alice, &f.source("alice")),
        Err(ServiceError::AlreadyBound)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(f.temp.path().join("service"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for dir in [a_dir, b_dir] {
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for file in ["config.yaml", "auth.json", "history.sqlite3"] {
                assert_eq!(
                    fs::metadata(dir.join(file)).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }
}

#[test]
fn identity_paths_and_user_request_overrides_are_rejected() {
    for id in [
        "../10001",
        "/tmp/user",
        "0010001",
        "0",
        "1e3",
        "1\n",
        "9223372036854775808",
    ] {
        assert!(Actor::qq("90001", id).is_err());
    }
    let f = Fixture::new();
    let other_bot = Actor::qq("90002", "10001").unwrap();
    assert!(matches!(
        f.service.status(&other_bot),
        Err(ServiceError::NotBound)
    ));
    f.service.provision(&other_bot, &f.source("alice")).unwrap();
    assert_ne!(f.user_dir(&f.alice), f.user_dir(&other_bot));
    for request in [
        json!({"action":"query","user_id":"10002"}),
        json!({"action":"query","config":"/tmp/shared.yaml"}),
        json!({"action":"history","limit":10,"bot_id":"90002"}),
        json!({"action":"preferences","patch":{"threshold_kwh":"5","qq":"10002"}}),
        json!({"action":"bind","password":"secret"}),
    ] {
        assert!(serde_json::from_value::<UserRequest>(request).is_err());
    }
    assert!(
        serde_json::from_value::<UserRequest>(
            json!({"action":"preferences","patch":{"threshold_kwh":"5"}})
        )
        .is_ok()
    );
}

#[test]
fn decimal_threshold_queue_and_acknowledged_cooldown_survive_restart() {
    let f = Fixture::new();
    let backend = Backend::new();
    backend.set("alice", Outcome::Energy("10.0000"));
    let event = f.service.query_with(&f.alice, &backend, 1000).unwrap();
    assert_eq!(event.low_balance, Some(true));
    assert_eq!(f.service.outbox(&f.alice).unwrap().len(), 1);
    let reopened = Service::open(&f.temp.path().join("service"), Duration::from_secs(1)).unwrap();
    reopened.query_with(&f.alice, &backend, 1010).unwrap();
    assert_eq!(reopened.outbox(&f.alice).unwrap().len(), 1);
    let first = reopened
        .claim_deliveries("90001", 10, 1010)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(first.actor, f.alice);
    assert!(
        reopened
            .claim_deliveries("90001", 10, 1011)
            .unwrap()
            .is_empty()
    );
    reopened.complete_delivery(&first, false, 1012).unwrap();
    assert!(
        reopened
            .claim_deliveries("90001", 10, 1071)
            .unwrap()
            .is_empty()
    );
    let second = reopened
        .claim_deliveries("90001", 10, 1072)
        .unwrap()
        .pop()
        .unwrap();
    assert_ne!(first.lease_token, second.lease_token);
    assert_eq!(second.attempts, 2);
    assert!(matches!(
        reopened.complete_delivery(&first, true, 1073),
        Err(ServiceError::InvalidLease)
    ));
    reopened.complete_delivery(&second, true, 1073).unwrap();
    reopened.query_with(&f.alice, &backend, 1080).unwrap();
    assert!(reopened.outbox(&f.alice).unwrap().is_empty());
    reopened
        .query_with(&f.alice, &backend, 1073 + 86400)
        .unwrap();
    assert_eq!(reopened.outbox(&f.alice).unwrap().len(), 1);
}

#[test]
fn leased_notifications_cannot_cross_users_and_expired_leases_are_reclaimed() {
    let f = Fixture::new();
    let backend = Backend::new();
    f.service.query_with(&f.alice, &backend, 1000).unwrap();
    let first = f
        .service
        .claim_deliveries("90001", 1, 1000)
        .unwrap()
        .pop()
        .unwrap();
    let mut forged = first.clone();
    forged.actor = f.bob.clone();
    assert!(matches!(
        f.service.complete_delivery(&forged, true, 1001),
        Err(ServiceError::InvalidLease)
    ));
    assert!(
        f.service
            .claim_deliveries("90002", 1, 1001)
            .unwrap()
            .is_empty()
    );
    let next = f
        .service
        .claim_deliveries("90001", 1, 1120)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(next.notification_id, first.notification_id);
    assert_ne!(next.lease_token, first.lease_token);
    assert!(matches!(
        f.service.complete_delivery(&first, true, 1121),
        Err(ServiceError::InvalidLease)
    ));
    f.service.complete_delivery(&next, true, 1121).unwrap();
}

#[test]
fn recovery_and_disabling_cancel_pending_and_in_flight_notifications() {
    let f = Fixture::new();
    let backend = Backend::new();
    f.service.query_with(&f.alice, &backend, 1000).unwrap();
    let old = f
        .service
        .claim_deliveries("90001", 1, 1000)
        .unwrap()
        .pop()
        .unwrap();
    backend.set("alice", Outcome::Energy("10.01"));
    f.service.query_with(&f.alice, &backend, 1010).unwrap();
    assert!(f.service.outbox(&f.alice).unwrap().is_empty());
    assert!(matches!(
        f.service.complete_delivery(&old, true, 1011),
        Err(ServiceError::InvalidLease)
    ));
    backend.set("alice", Outcome::Energy("7"));
    f.service.query_with(&f.alice, &backend, 1020).unwrap();
    assert_eq!(f.service.outbox(&f.alice).unwrap().len(), 1);
    f.service
        .preferences(
            &f.alice,
            PreferencePatch {
                enabled: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(f.service.outbox(&f.alice).unwrap().is_empty());
    backend.set("alice", Outcome::Auth);
    f.service.query_with(&f.alice, &backend, 1030).unwrap();
    assert!(f.service.outbox(&f.alice).unwrap().is_empty());
}

#[test]
fn only_verified_users_are_scheduled_and_one_account_failure_does_not_stop_others() {
    let f = Fixture::new();
    let backend = Backend::new();
    assert_eq!(f.service.tick_with(&backend, 1000).unwrap().checked, 0);
    for actor in [&f.alice, &f.bob] {
        enable_interval(&f.service, actor);
        f.service.query_with(actor, &backend, 1000).unwrap();
    }
    assert_eq!(f.service.tick_with(&backend, 1299).unwrap().checked, 0);
    backend.set("alice", Outcome::Auth);
    let report = f.service.tick_with(&backend, 1300).unwrap();
    assert_eq!((report.checked, report.failed), (2, 1));
    assert_eq!(f.service.status(&f.alice).unwrap().state, "needs_auth");
    assert_eq!(f.service.status(&f.bob).unwrap().state, "active");
    assert_eq!(f.service.outbox(&f.alice).unwrap()[0].topic, "auth");
    assert!(
        f.service.history(&f.alice, 20).unwrap()[0]
            .remaining_kwh
            .is_none()
    );
    let report = f.service.tick_with(&backend, 1600).unwrap();
    assert_eq!(report.checked, 1);
    assert_eq!(f.service.history(&f.alice, 20).unwrap().len(), 2);
}

#[test]
fn transient_errors_retry_without_zero_balance_or_auth_pause() {
    let f = Fixture::new();
    let backend = Backend::new();
    f.service.query_with(&f.bob, &backend, 1000).unwrap();
    backend.set("bob", Outcome::Network);
    let report = f.service.tick_with(&backend, 22600).unwrap();
    assert_eq!(report.failed, 1);
    let state = f.service.status(&f.bob).unwrap();
    assert_eq!(state.state, "active");
    assert_eq!(state.next_check_at, 22900);
    assert!(
        f.service.history(&f.bob, 20).unwrap()[0]
            .remaining_kwh
            .is_none()
    );
    assert!(f.service.outbox(&f.bob).unwrap().is_empty());
    assert_eq!(f.service.tick_with(&backend, 22899).unwrap().checked, 0);
    backend.set("bob", Outcome::Energy("30"));
    assert_eq!(f.service.tick_with(&backend, 22900).unwrap().failed, 0);
}

#[test]
fn rate_limits_and_invalid_preferences_do_not_affect_other_users() {
    let f = Fixture::new();
    let backend = Backend::new();
    f.service.query_with(&f.alice, &backend, 1000).unwrap();
    assert!(matches!(
        f.service.query_with(&f.alice, &backend, 1001),
        Err(ServiceError::RateLimited)
    ));
    assert!(f.service.query_with(&f.bob, &backend, 1001).is_ok());
    for value in ["-1", "abc", "10001"] {
        assert!(
            f.service
                .preferences(
                    &f.alice,
                    PreferencePatch {
                        threshold_kwh: Some(value.into()),
                        ..Default::default()
                    }
                )
                .is_err()
        );
    }
    assert!(
        f.service
            .preferences(
                &f.alice,
                PreferencePatch {
                    interval_seconds: Some(1),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(
        f.service
            .status(&f.alice)
            .unwrap()
            .preferences
            .threshold_kwh,
        "10"
    );
}

#[test]
fn unbinding_erases_one_user_and_old_deliveries_never_reach_rebound_user() {
    let f = Fixture::new();
    let backend = Backend::new();
    let old_dir = f.user_dir(&f.alice);
    f.service.query_with(&f.alice, &backend, 1000).unwrap();
    let old = f
        .service
        .claim_deliveries("90001", 1, 1000)
        .unwrap()
        .pop()
        .unwrap();
    f.service.query_with(&f.bob, &backend, 1000).unwrap();
    f.service.unbind(&f.alice).unwrap();
    assert!(!old_dir.exists());
    assert!(matches!(
        f.service.status(&f.alice),
        Err(ServiceError::NotBound)
    ));
    assert_eq!(f.service.history(&f.bob, 20).unwrap().len(), 1);
    f.service.provision(&f.alice, &f.source("alice")).unwrap();
    assert_ne!(f.user_dir(&f.alice), old_dir);
    assert!(f.service.history(&f.alice, 20).unwrap().is_empty());
    assert!(matches!(
        f.service.complete_delivery(&old, true, 1001),
        Err(ServiceError::InvalidLease)
    ));
}

#[test]
fn deletion_tombstones_are_inaccessible_and_cleaned_after_restart() {
    let f = Fixture::new();
    let dir = f.user_dir(&f.alice);
    let db = Connection::open(f.temp.path().join("service/registry.sqlite3")).unwrap();
    db.execute(
        "UPDATE bindings SET state='deleting' WHERE user_id=?1",
        [f.alice.user_id()],
    )
    .unwrap();
    assert!(matches!(
        f.service.status(&f.alice),
        Err(ServiceError::NotBound)
    ));
    let reopened = Service::open(&f.temp.path().join("service"), Duration::from_secs(1)).unwrap();
    assert!(!dir.exists());
    assert!(reopened.provision(&f.alice, &f.source("alice")).is_ok());
    let bob_dir = f.user_dir(&f.bob);
    db.execute(
        "UPDATE bindings SET state='provisioning' WHERE user_id=?1",
        [f.bob.user_id()],
    )
    .unwrap();
    assert!(matches!(
        f.service.status(&f.bob),
        Err(ServiceError::NotBound)
    ));
    let restarted = Service::open(&f.temp.path().join("service"), Duration::from_secs(1)).unwrap();
    assert!(!bob_dir.exists());
    assert!(restarted.provision(&f.bob, &f.source("bob")).is_ok());
}

struct BlockingBackend {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
    backend: Backend,
}
impl ElectricityBackend for BlockingBackend {
    fn read(&self, path: &Path, timeout: Duration) -> Result<SchoolReading, QueryError> {
        if Settings::load(path)?.cas.unwrap().username == "alice" {
            self.entered.wait();
            self.release.wait();
        }
        self.backend.read(path, timeout)
    }
}
#[test]
fn concurrent_operations_lock_only_the_same_user() {
    let f = Fixture::new();
    let backend = Arc::new(BlockingBackend {
        entered: Arc::new(Barrier::new(2)),
        release: Arc::new(Barrier::new(2)),
        backend: Backend::new(),
    });
    let service = f.service.clone();
    let actor = f.alice.clone();
    let worker = backend.clone();
    let thread = std::thread::spawn(move || service.query_with(&actor, worker.as_ref(), 1000));
    backend.entered.wait();
    assert!(matches!(
        f.service.status(&f.alice),
        Err(ServiceError::Busy)
    ));
    assert!(f.service.query_with(&f.bob, backend.as_ref(), 1000).is_ok());
    assert!(matches!(
        f.service.unbind(&f.alice),
        Err(ServiceError::Busy)
    ));
    backend.release.wait();
    assert!(thread.join().unwrap().is_ok());
}

#[test]
fn two_schedulers_cannot_query_the_same_due_user_twice() {
    let f = Fixture::new();
    let initial = Backend::new();
    for actor in [&f.alice, &f.bob] {
        enable_interval(&f.service, actor);
        f.service.query_with(actor, &initial, 1000).unwrap();
    }
    let backend = Arc::new(BlockingBackend {
        entered: Arc::new(Barrier::new(2)),
        release: Arc::new(Barrier::new(2)),
        backend: Backend::new(),
    });
    let service = f.service.clone();
    let worker = backend.clone();
    let thread = std::thread::spawn(move || service.tick_with(worker.as_ref(), 1300));
    backend.entered.wait();
    let second = f.service.tick_with(backend.as_ref(), 1300).unwrap();
    assert!(second.skipped >= 1);
    backend.release.wait();
    let first = thread.join().unwrap().unwrap();
    assert_eq!(first.checked + second.checked, 2);
    for actor in [&f.alice, &f.bob] {
        assert_eq!(f.service.history(actor, 20).unwrap().len(), 2);
    }
}

#[test]
fn authentication_import_cancels_stale_notices_until_the_target_is_reverified() {
    let f = Fixture::new();
    let backend = Backend::new();
    f.service.query_with(&f.alice, &backend, 1000).unwrap();
    let delivery = f
        .service
        .claim_deliveries("90001", 1, 1000)
        .unwrap()
        .pop()
        .unwrap();
    let input = f.temp.path().join("input.json");
    auth::secure_write(&input, b"{\"access_token\":\"fake-new-token\"}").unwrap();
    f.service
        .import_auth(&f.alice, &input, auth::OAuthProvider::Berserker, None)
        .unwrap();
    assert_eq!(f.service.status(&f.alice).unwrap().state, "pending");
    assert!(f.service.outbox(&f.alice).unwrap().is_empty());
    assert!(matches!(
        f.service.complete_delivery(&delivery, true, 1001),
        Err(ServiceError::InvalidLease)
    ));
    assert_eq!(f.service.tick_with(&backend, 100000).unwrap().checked, 0);
}

#[test]
fn napcat_contract_uses_authenticated_private_event_actor_and_plain_text_recipient() {
    let base = json!({"post_type":"message","message_type":"private","sub_type":"friend","self_id":90001,"user_id":10001,"sender":{"user_id":10002}});
    assert_eq!(
        napcat::actor_from_event(&base, "90001").unwrap(),
        Actor::qq("90001", "10001").unwrap()
    );
    for (key, value) in [
        ("post_type", json!("message_sent")),
        ("message_type", json!("group")),
        ("sub_type", json!("group")),
        ("self_id", json!(90002)),
    ] {
        let mut event = base.clone();
        event[key] = value;
        assert!(napcat::actor_from_event(&event, "90001").is_err());
    }
    let delivery = Delivery {
        actor: Actor::qq("90001", "10001").unwrap(),
        notification_id: "id".into(),
        lease_token: "lease".into(),
        message: "[CQ:at,qq=all]".into(),
        attempts: 1,
    };
    let body = napcat::private_message(&delivery);
    assert_eq!(body["params"]["user_id"], "10001");
    assert_eq!(body["params"]["message"][0]["type"], "text");
    assert!(body.get("lease_token").is_none());
}

struct RecordingSink {
    success: bool,
    recipients: Vec<String>,
}
impl NotificationSink for RecordingSink {
    fn send_private(&mut self, delivery: &Delivery) -> Result<String, SendError> {
        self.recipients.push(delivery.actor.user_id().into());
        if self.success {
            Ok("message-id".into())
        } else {
            Err(SendError)
        }
    }
}
#[test]
fn transport_failure_retains_queue_and_success_is_the_only_delivery_confirmation() {
    let f = Fixture::new();
    let backend = Backend::new();
    let now = chrono::Utc::now().timestamp();
    f.service.query_with(&f.alice, &backend, now).unwrap();
    let mut sink = RecordingSink {
        success: false,
        recipients: Vec::new(),
    };
    assert_eq!(f.service.dispatch("90001", &mut sink).unwrap(), 0);
    assert_eq!(sink.recipients, ["10001"]);
    assert_eq!(f.service.outbox(&f.alice).unwrap().len(), 1);
    let db = Connection::open(f.user_dir(&f.alice).join("history.sqlite3")).unwrap();
    db.execute("UPDATE outbox SET retry_at=0", []).unwrap();
    sink.success = true;
    assert_eq!(f.service.dispatch("90001", &mut sink).unwrap(), 1);
    assert!(f.service.outbox(&f.alice).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn symlinked_tenant_directories_are_not_followed() {
    let f = Fixture::new();
    let dir = f.user_dir(&f.alice);
    let saved = f.temp.path().join("saved-user");
    fs::rename(&dir, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &dir).unwrap();
    assert!(matches!(
        f.service.status(&f.alice),
        Err(ServiceError::Storage)
    ));
    assert!(saved.join("config.yaml").exists());
}
