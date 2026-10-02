use rust_decimal::Decimal;
use sdu_infohelper::{
    Location, QueryError, Reading,
    auth::LoginOptions,
    monitor::{Comparison, query_source, query_source_with_context},
    source::{
        AirconSource, DormSource, ElectricitySource, ReadContext, SourceError, SourceReading,
    },
};
use std::{cell::Cell, collections::BTreeMap, path::Path, str::FromStr, time::Duration};

fn location() -> Location {
    Location {
        campus: "1&威海".into(),
        building: "2&二".into(),
        floor: "3".into(),
        room: "301".into(),
    }
}

struct FakeSource<'a> {
    calls: Cell<usize>,
    cancel_after: Option<&'a Cell<bool>>,
    failure: bool,
}
impl ElectricitySource for FakeSource<'_> {
    type Target = Location;
    fn read(&self, _: &ReadContext<'_>) -> Result<SourceReading<Location>, SourceError<Location>> {
        self.calls.set(self.calls.get() + 1);
        if let Some(cancelled) = self.cancel_after {
            cancelled.set(true);
        }
        if self.failure {
            return Err(SourceError::Failed {
                error: QueryError::Authentication,
                target: Some(location()),
            });
        }
        Ok(SourceReading {
            reading: Reading {
                remaining_kwh: Decimal::from_str("10.00100").unwrap(),
                supply_status: Some("正常".into()),
            },
            target: location(),
            token_expires_at_claim: Some("2027-01-01".into()),
        })
    }
}

#[test]
fn injected_source_preserves_precision_comparison_and_failure_context() {
    let mut source = FakeSource {
        calls: Cell::new(0),
        cancel_after: None,
        failure: false,
    };
    let threshold = Some(Decimal::from_str("10.001").unwrap());
    let strict = query_source(
        &source,
        Duration::from_secs(1),
        threshold,
        Comparison::StrictlyBelow,
    );
    let inclusive = query_source(
        &source,
        Duration::from_secs(1),
        threshold,
        Comparison::Inclusive,
    );
    assert_eq!(strict.event.remaining_kwh.as_deref(), Some("10.00100"));
    assert_eq!(strict.event.low_balance, Some(false));
    assert_eq!(inclusive.event.low_balance, Some(true));
    assert!(!strict.fatal);
    source.failure = true;
    let failure = query_source(
        &source,
        Duration::from_secs(1),
        threshold,
        Comparison::Inclusive,
    );
    assert!(failure.fatal && failure.needs_login);
    assert_eq!(failure.event.location.unwrap().room, "301");
    assert!(failure.event.remaining_kwh.is_none() && failure.event.low_balance.is_none());
}

#[test]
fn cancellation_before_read_avoids_source_and_after_read_discards_sample() {
    let cancelled = Cell::new(true);
    let check = || Ok(cancelled.get());
    let context = ReadContext::with_cancellation(Duration::from_secs(1), &check);
    let source = FakeSource {
        calls: Cell::new(0),
        cancel_after: Some(&cancelled),
        failure: false,
    };
    assert!(query_source_with_context(&source, &context, None, Comparison::Inclusive).is_none());
    assert_eq!(source.calls.get(), 0);
    cancelled.set(false);
    assert!(query_source_with_context(&source, &context, None, Comparison::Inclusive).is_none());
    assert_eq!(source.calls.get(), 1);
}

#[test]
fn concrete_sources_cancel_before_reading_config_or_authentication() {
    let check = || Ok(true);
    let context = ReadContext::with_cancellation(Duration::from_secs(1), &check);
    let missing = Path::new("/definitely-missing-sdu-source-config.yaml");
    let overrides = BTreeMap::new();
    assert!(matches!(
        DormSource::new(missing, &overrides).read(&context),
        Err(SourceError::Cancelled)
    ));
    assert!(matches!(
        AirconSource::new(missing, LoginOptions::default()).read(&context),
        Err(SourceError::Cancelled)
    ));
}

#[test]
fn concrete_sources_fail_locally_without_credentials_or_selected_aircon() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    std::fs::write(&config, "{}\n").unwrap();
    let context = ReadContext::new(Duration::from_secs(1));
    let overrides = BTreeMap::new();
    assert!(matches!(
        DormSource::new(&config, &overrides).read(&context),
        Err(SourceError::Failed {
            error: QueryError::AuthFlow(_),
            ..
        })
    ));
    assert!(matches!(
        AirconSource::new(&config, LoginOptions::default()).read(&context),
        Err(SourceError::Failed {
            error: QueryError::Config(_),
            ..
        })
    ));
}
