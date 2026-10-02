//! Shared single-query logic and channel-independent monitor state.
use crate::{
    Event, Location, QueryError,
    control::OperationControl,
    source::{DormSource, ElectricitySource, ReadContext, SourceError},
};
use rust_decimal::Decimal;
use std::{collections::BTreeMap, path::Path, time::Duration};

pub mod delivery;
mod schedule;
pub use schedule::{Cadence, Schedule};
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
    observe_source(
        &DormSource::new(path, overrides),
        &ReadContext::from_control(control),
        threshold,
        comparison,
    )
}

/// Sample a resolved dorm-shaped source using the existing event and threshold contract.
pub fn query_source<S: ElectricitySource<Target = Location> + ?Sized>(
    source: &S,
    timeout: Duration,
    threshold: Option<Decimal>,
    comparison: Comparison,
) -> Sample {
    query_source_with_context(source, &ReadContext::new(timeout), threshold, comparison)
        .unwrap_or_else(|| {
            Observation::failure(QueryError::Response("查询已取消。"), None).into_sample()
        })
}

/// Cancelled operations produce no sample, history entry or notification.
pub fn query_source_with_context<S: ElectricitySource<Target = Location> + ?Sized>(
    source: &S,
    context: &ReadContext<'_>,
    threshold: Option<Decimal>,
    comparison: Comparison,
) -> Option<Sample> {
    observe_source(source, context, threshold, comparison).map(Observation::into_sample)
}

fn observe_source<S: ElectricitySource<Target = Location> + ?Sized>(
    source: &S,
    context: &ReadContext<'_>,
    threshold: Option<Decimal>,
    comparison: Comparison,
) -> Option<Observation> {
    let result = context.check().and_then(|()| source.read(context));
    let result = match context.check() {
        Ok(()) => result,
        Err(error) => Err(error),
    };
    Some(match result {
        Ok(value) => Observation::success(
            value.reading,
            value.target,
            threshold,
            comparison,
            value.token_expires_at_claim,
        ),
        Err(SourceError::Cancelled) => return None,
        Err(SourceError::Failed { error, target }) => Observation::failure(error, target),
    })
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
