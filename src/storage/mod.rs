//! SQLite storage facade; schema and SQL are kept outside CLI and monitor policy.
pub(crate) mod delivery;
mod history;
mod migrations;
pub use history::{
    HistoryRecord, HistoryStore, alert_due, clear_alert, history, mark_alert, save_event,
};
