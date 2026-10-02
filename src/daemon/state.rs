//! Persistent daemon status contract.
use crate::{
    Location, Reading,
    monitor::{
        delivery::ChannelState,
        observation::{FailureDisposition, Observation, Outcome},
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Deserialize, Serialize)]
pub struct LastSuccess {
    pub checked_at: String,
    pub remaining_kwh: String,
    pub location: Option<Location>,
    pub supply_status: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct RuntimeState {
    pub run_id: String,
    pub pid: u32,
    #[serde(default)]
    pub executable: Option<PathBuf>,
    pub status: String,
    pub started_at: String,
    pub last_attempt_at: Option<String>,
    pub last_success: Option<LastSuccess>,
    pub next_check_at: Option<String>,
    pub last_error: Option<String>,
    pub channels: Vec<ChannelState>,
    #[serde(default)]
    pub recent_starts: Vec<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkerStatus {
    Starting,
    Checking,
    Healthy,
    LowBalance,
    NeedsLogin,
    QueryFailed,
    Stopped,
    Failed,
    Terminated,
}

impl WorkerStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Checking => "checking",
            Self::Healthy => "healthy",
            Self::LowBalance => "low_balance",
            Self::NeedsLogin => "needs_login",
            Self::QueryFailed => "query_failed",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Terminated => "terminated",
        }
    }
}

enum SuccessRecord {
    // Keep an older snapshot byte-for-byte until a fresh typed reading replaces it.
    Retained(LastSuccess),
    Observed {
        checked_at: DateTime<Utc>,
        reading: Reading,
        location: Location,
    },
}

impl SuccessRecord {
    fn snapshot(&self) -> LastSuccess {
        match self {
            Self::Retained(snapshot) => snapshot.clone(),
            Self::Observed {
                checked_at,
                reading,
                location,
            } => LastSuccess {
                checked_at: checked_at.to_rfc3339(),
                remaining_kwh: reading.remaining_kwh.to_string(),
                location: Some(location.clone()),
                supply_status: reading.supply_status.clone(),
            },
        }
    }
}

/// The worker mutates typed state; RuntimeState remains the compatibility file/JSON DTO.
pub(super) struct WorkerState {
    pub run_id: String,
    pid: u32,
    executable: Option<PathBuf>,
    pub status: WorkerStatus,
    started_at: DateTime<Utc>,
    last_attempt_at: Option<DateTime<Utc>>,
    last_success: Option<SuccessRecord>,
    pub next_check_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub channels: Vec<ChannelState>,
    pub recent_starts: Vec<i64>,
}

impl WorkerState {
    pub(super) fn new(
        run_id: String,
        started_at: DateTime<Utc>,
        previous: Option<LastSuccess>,
        recent_starts: Vec<i64>,
    ) -> Self {
        Self {
            run_id,
            pid: std::process::id(),
            executable: std::env::current_exe().ok(),
            status: WorkerStatus::Starting,
            started_at,
            last_attempt_at: None,
            last_success: previous.map(SuccessRecord::Retained),
            next_check_at: None,
            last_error: None,
            channels: Vec::new(),
            recent_starts,
        }
    }

    pub(super) fn observe(&mut self, observation: &Observation) {
        self.last_attempt_at = Some(observation.checked_at);
        match &observation.outcome {
            Outcome::Success(success) => {
                self.last_error = None;
                self.last_success = Some(SuccessRecord::Observed {
                    checked_at: observation.checked_at,
                    reading: success.reading.clone(),
                    location: success.location.clone(),
                });
                self.status = if success.low_balance() == Some(true) {
                    WorkerStatus::LowBalance
                } else {
                    WorkerStatus::Healthy
                };
            }
            Outcome::Failure {
                error, disposition, ..
            } => {
                self.last_error = Some(error.to_string());
                self.status = if *disposition == FailureDisposition::NeedsLogin {
                    WorkerStatus::NeedsLogin
                } else {
                    WorkerStatus::QueryFailed
                };
            }
        }
    }

    pub(super) fn finish(&mut self, result: &Result<(), String>) {
        self.status = if result.is_ok() {
            WorkerStatus::Stopped
        } else {
            WorkerStatus::Failed
        };
        self.next_check_at = None;
        if let Err(error) = result {
            self.last_error = Some(error.clone());
        }
    }

    pub(super) fn snapshot(&self) -> RuntimeState {
        RuntimeState {
            run_id: self.run_id.clone(),
            pid: self.pid,
            executable: self.executable.clone(),
            status: self.status.as_str().into(),
            started_at: self.started_at.to_rfc3339(),
            last_attempt_at: self.last_attempt_at.map(|time| time.to_rfc3339()),
            last_success: self.last_success.as_ref().map(SuccessRecord::snapshot),
            next_check_at: self.next_check_at.map(|time| time.to_rfc3339()),
            last_error: self.last_error.clone(),
            channels: self.channels.clone(),
            recent_starts: self.recent_starts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{QueryError, monitor::Comparison};

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
            Some("10".parse().unwrap()),
            Comparison::StrictlyBelow,
            None,
        )
    }

    #[test]
    fn failed_checks_retain_precise_last_success_and_recovery_clears_the_error() {
        let mut state = WorkerState::new("run".into(), Utc::now(), None, Vec::new());
        state.observe(&reading("9.500"));
        assert_eq!(state.snapshot().status, "low_balance");
        state.observe(&Observation::failure(QueryError::Network, None));
        assert_eq!(state.snapshot().status, "query_failed");
        assert_eq!(
            state.snapshot().last_success.unwrap().remaining_kwh,
            "9.500"
        );
        state.observe(&Observation::failure(QueryError::Authentication, None));
        assert_eq!(state.snapshot().status, "needs_login");
        state.observe(&reading("10.000"));
        assert_eq!(state.snapshot().status, "healthy");
        assert!(state.snapshot().last_error.is_none());
        assert_eq!(
            state.snapshot().last_success.unwrap().remaining_kwh,
            "10.000"
        );
        state.next_check_at = Some(Utc::now());
        state.finish(&Ok(()));
        assert_eq!(state.snapshot().status, "stopped");
        assert!(state.snapshot().next_check_at.is_none());
    }

    #[test]
    fn previously_stored_success_is_not_reparsed_or_discarded_during_startup_failure() {
        let previous = LastSuccess {
            checked_at: "legacy timestamp".into(),
            remaining_kwh: "00010.00".into(),
            location: None,
            supply_status: Some("legacy".into()),
        };
        let expected = serde_json::to_value(&previous).unwrap();
        let mut state = WorkerState::new("run".into(), Utc::now(), Some(previous), Vec::new());
        state.observe(&Observation::failure(QueryError::Authentication, None));
        state.finish(&Err("配置错误".into()));
        let snapshot = state.snapshot();
        assert_eq!(snapshot.status, "failed");
        assert_eq!(snapshot.last_error.as_deref(), Some("配置错误"));
        assert_eq!(
            serde_json::to_value(snapshot.last_success.unwrap()).unwrap(),
            expected
        );
    }
}
