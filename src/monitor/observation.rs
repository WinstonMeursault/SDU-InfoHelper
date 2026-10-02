//! Typed internal query result; output strings are produced only by boundary adapters.
use super::{Comparison, Sample};
use crate::{Event, Location, QueryError, Reading};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureDisposition {
    Retry,
    NeedsLogin,
    Fatal,
}

impl FailureDisposition {
    pub(crate) fn of(error: &QueryError) -> Self {
        match error {
            QueryError::Authentication | QueryError::AuthFlow(_) => Self::NeedsLogin,
            QueryError::Config(_) => Self::Fatal,
            _ => Self::Retry,
        }
    }

    pub(crate) fn flags(self) -> (bool, bool) {
        (self != Self::Retry, self == Self::NeedsLogin)
    }
}

pub(crate) struct SuccessfulObservation {
    pub reading: Reading,
    pub location: Location,
    pub threshold: Option<Decimal>,
    pub comparison: Comparison,
    pub expiry: Option<String>,
}

impl SuccessfulObservation {
    pub(crate) fn low_balance(&self) -> Option<bool> {
        self.threshold
            .map(|threshold| self.comparison.low(self.reading.remaining_kwh, threshold))
    }
}

pub(crate) enum Outcome {
    Success(SuccessfulObservation),
    Failure {
        error: QueryError,
        location: Option<Location>,
        disposition: FailureDisposition,
    },
}

pub(crate) struct Observation {
    pub checked_at: DateTime<Utc>,
    pub outcome: Outcome,
}

impl Observation {
    pub(crate) fn success(
        reading: Reading,
        location: Location,
        threshold: Option<Decimal>,
        comparison: Comparison,
        expiry: Option<String>,
    ) -> Self {
        Self {
            checked_at: Utc::now(),
            outcome: Outcome::Success(SuccessfulObservation {
                reading,
                location,
                threshold,
                comparison,
                expiry,
            }),
        }
    }

    pub(crate) fn failure(error: QueryError, location: Option<Location>) -> Self {
        let disposition = FailureDisposition::of(&error);
        Self {
            checked_at: Utc::now(),
            outcome: Outcome::Failure {
                error,
                location,
                disposition,
            },
        }
    }

    pub(crate) fn flags(&self) -> (bool, bool) {
        match &self.outcome {
            Outcome::Success(_) => (false, false),
            Outcome::Failure { disposition, .. } => disposition.flags(),
        }
    }

    pub(crate) fn message(&self) -> String {
        match &self.outcome {
            Outcome::Success(success) => format!("剩余电量 {} 度。", success.reading.remaining_kwh),
            Outcome::Failure { error, .. } => error.to_string(),
        }
    }

    pub(crate) fn into_sample(self) -> Sample {
        let (fatal, needs_login) = self.flags();
        Sample {
            event: Event::from(&self),
            fatal,
            needs_login,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn location() -> Location {
        Location {
            campus: "C&校区".into(),
            building: "B&8".into(),
            floor: "F&4".into(),
            room: "R&405".into(),
        }
    }

    #[test]
    fn typed_success_keeps_decimal_scale_and_the_existing_json_contract() {
        for (comparison, low) in [
            (Comparison::Inclusive, true),
            (Comparison::StrictlyBelow, false),
        ] {
            let mut observation = Observation::success(
                Reading {
                    remaining_kwh: "10.00".parse().unwrap(),
                    supply_status: Some("查询失败".into()),
                },
                location(),
                Some("10.000".parse().unwrap()),
                comparison,
                Some("expiry-claim".into()),
            );
            observation.checked_at = "2026-10-02T00:00:00Z".parse().unwrap();
            let event = Event::from(&observation);
            assert_eq!(
                serde_json::to_value(&event).unwrap(),
                json!({
                    "checked_at":"2026-10-02T00:00:00+00:00", "remaining_kwh":"10.00", "unit":"kWh",
                    "supply_status":"查询失败", "threshold_kwh":"10.000", "low_balance":low,
                    "token_expires_at_claim":"expiry-claim", "error":null,
                    "location":{"campus":"C&校区","building":"B&8","floor":"F&4","room":"R&405"}
                })
            );
        }
    }

    #[test]
    fn typed_failures_have_no_balance_and_preserve_attention_policy() {
        for (error, flags) in [
            (QueryError::Timeout, (false, false)),
            (QueryError::AuthFlow("需要本机登录"), (true, true)),
            (QueryError::Config("配置错误"), (true, false)),
        ] {
            let observation = Observation::failure(error, Some(location()));
            assert_eq!(observation.flags(), flags);
            let event = Event::from(&observation);
            assert!(event.remaining_kwh.is_none());
            assert!(event.threshold_kwh.is_none());
            assert!(event.low_balance.is_none());
            assert!(event.token_expires_at_claim.is_none());
            assert_eq!(event.location.unwrap().room, "R&405");
        }
    }
}
