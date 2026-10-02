//! Stable query output shared by CLI, history and notifications.
use crate::monitor::observation::{Observation, Outcome};
use crate::{Location, QueryError, Reading};
use chrono::Utc;
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Event {
    pub checked_at: String,
    pub remaining_kwh: Option<String>,
    pub unit: &'static str,
    pub supply_status: Option<String>,
    pub threshold_kwh: Option<String>,
    pub low_balance: Option<bool>,
    pub token_expires_at_claim: Option<String>,
    pub error: Option<String>,
    pub location: Option<Location>,
}

impl From<&Observation> for Event {
    fn from(observation: &Observation) -> Self {
        match &observation.outcome {
            Outcome::Success(success) => Self {
                checked_at: observation.checked_at.to_rfc3339(),
                remaining_kwh: Some(success.reading.remaining_kwh.to_string()),
                unit: "kWh",
                supply_status: success.reading.supply_status.clone(),
                threshold_kwh: success.threshold.map(|value| value.to_string()),
                low_balance: success.low_balance(),
                token_expires_at_claim: success.expiry.clone(),
                error: None,
                location: Some(success.location.clone()),
            },
            Outcome::Failure {
                error, location, ..
            } => Self {
                checked_at: observation.checked_at.to_rfc3339(),
                remaining_kwh: None,
                unit: "kWh",
                supply_status: None,
                threshold_kwh: None,
                low_balance: None,
                token_expires_at_claim: None,
                error: Some(error.to_string()),
                location: location.clone(),
            },
        }
    }
}

impl Event {
    pub fn success(reading: Reading, threshold: Option<Decimal>, expiry: Option<String>) -> Self {
        Self {
            checked_at: Utc::now().to_rfc3339(),
            remaining_kwh: Some(reading.remaining_kwh.to_string()),
            unit: "kWh",
            supply_status: reading.supply_status,
            threshold_kwh: threshold.map(|value| value.to_string()),
            low_balance: threshold.map(|value| reading.remaining_kwh <= value),
            token_expires_at_claim: expiry,
            error: None,
            location: None,
        }
    }

    pub fn failure(error: &QueryError) -> Self {
        Self {
            checked_at: Utc::now().to_rfc3339(),
            remaining_kwh: None,
            unit: "kWh",
            supply_status: None,
            threshold_kwh: None,
            low_balance: None,
            token_expires_at_claim: None,
            error: Some(error.to_string()),
            location: None,
        }
    }

    pub fn at_location(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }
}
