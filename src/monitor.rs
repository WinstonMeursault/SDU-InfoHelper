//! Shared single-query logic and channel-independent monitor state.
use crate::{
    Event, QueryError,
    control::{OperationControl, OperationError},
    dorm::with_dorm_auth_controlled,
    query,
};
use rust_decimal::Decimal;
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

pub mod delivery;
pub(crate) mod observation;
use observation::{FailureDisposition, Observation};

#[derive(Clone, Copy)]
pub enum Comparison {
    Inclusive,
    StrictlyBelow,
}

impl Comparison {
    pub fn low(self, remaining: Decimal, threshold: Decimal) -> bool {
        match self {
            Self::Inclusive => remaining <= threshold,
            Self::StrictlyBelow => remaining < threshold,
        }
    }
}

pub struct Sample {
    pub event: Event,
    pub fatal: bool,
    pub needs_login: bool,
}

pub fn failure_flags(error: &QueryError) -> (bool, bool) {
    FailureDisposition::of(error).flags()
}

pub fn query_once(
    path: &Path,
    timeout: Duration,
    threshold: Option<Decimal>,
    overrides: &BTreeMap<String, String>,
    comparison: Comparison,
) -> Sample {
    query_once_controlled(
        path,
        &OperationControl::uninterrupted(timeout),
        threshold,
        overrides,
        comparison,
    )
    .expect("an uninterrupted query cannot be cancelled")
}

pub(crate) fn query_once_controlled(
    path: &Path,
    control: &OperationControl<'_>,
    threshold: Option<Decimal>,
    overrides: &BTreeMap<String, String>,
    comparison: Comparison,
) -> Option<Sample> {
    observe_once_controlled(path, control, threshold, overrides, comparison)
        .map(Observation::into_sample)
}

pub(crate) fn observe_once_controlled(
    path: &Path,
    control: &OperationControl<'_>,
    threshold: Option<Decimal>,
    overrides: &BTreeMap<String, String>,
    comparison: Comparison,
) -> Option<Observation> {
    let mut target = None;
    let result = with_dorm_auth_controlled(path, control, overrides, |config, connection| {
        if config.form.values().any(|value| value == "_") {
            return Err(QueryError::Config(
                "请配置完整 dorm_electricity 目标，或使用 --campus --building --floor --room 指定。",
            ));
        }
        let location = config.location()?;
        target = Some(location.clone());
        let reading = query(config, connection)?;
        Ok(Observation::success(
            reading,
            location,
            threshold,
            comparison,
            config.expiry_claim(),
        ))
    });
    Some(match result {
        Ok(observation) => observation,
        Err(OperationError::Cancelled) => return None,
        Err(OperationError::Query(error)) => Observation::failure(error, target),
    })
}

/// Single-thread schedule: skip missed intervals, and also detect sleep on clocks
/// whose monotonic timer does not advance while the machine is suspended.
pub struct Schedule {
    interval: Duration,
    next: Instant,
    next_wall: i64,
    last_wall: i64,
}

impl Schedule {
    pub fn new(interval_seconds: u64, now: Instant, wall: i64) -> Self {
        Self {
            interval: Duration::from_secs(interval_seconds),
            next: now,
            next_wall: wall,
            last_wall: wall,
        }
    }
    pub fn due(&self, now: Instant, wall: i64) -> bool {
        now >= self.next || wall >= self.next_wall || wall < self.last_wall
    }
    pub fn started(&mut self, now: Instant, wall: i64) {
        self.next = now + self.interval;
        self.next_wall = wall.saturating_add(self.interval.as_secs() as i64);
        self.last_wall = wall;
    }
    pub fn next_wall(&self) -> i64 {
        self.next_wall
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn daemon_is_strict_and_legacy_comparison_stays_inclusive() {
        let threshold = Decimal::from_str("10.001").unwrap();
        for (remaining, strict, inclusive) in [
            ("10.0009", true, true),
            ("10.001", false, true),
            ("10.0011", false, false),
            ("0", true, true),
            ("-0.001", true, true),
        ] {
            let remaining = Decimal::from_str(remaining).unwrap();
            assert_eq!(Comparison::StrictlyBelow.low(remaining, threshold), strict);
            assert_eq!(Comparison::Inclusive.low(remaining, threshold), inclusive);
        }
    }

    #[test]
    fn schedule_checks_immediately_skips_missed_intervals_and_handles_sleep() {
        let now = Instant::now();
        let mut schedule = Schedule::new(60, now, 1000);
        assert!(schedule.due(now, 1000));
        schedule.started(now, 1000);
        assert!(!schedule.due(now + Duration::from_secs(59), 1059));
        assert!(schedule.due(now + Duration::from_secs(1), 1200));
        assert!(schedule.due(now + Duration::from_secs(1), 900));
        schedule.started(now + Duration::from_secs(180), 1180);
        assert!(!schedule.due(now + Duration::from_secs(180), 1180));
        assert_eq!(schedule.next_wall(), 1240);
    }

    #[test]
    fn background_query_without_credentials_reports_login_without_reading_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        std::fs::write(&config, "{}\n").unwrap();
        let sample = query_once(
            &config,
            Duration::from_secs(1),
            Some(Decimal::from(10)),
            &BTreeMap::new(),
            Comparison::StrictlyBelow,
        );
        assert!(sample.needs_login);
        assert!(sample.event.remaining_kwh.is_none());
        assert!(sample.event.low_balance.is_none());
    }
}
