//! Persistent daemon status contract.
use crate::{Location, monitor::delivery::ChannelState};
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
