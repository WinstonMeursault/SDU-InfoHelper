//! Public API facade. Protocol, orchestration, models and storage have separate modules.
pub mod aircon;
pub mod auth;
mod cas;
mod cas_des;
mod control;
pub mod daemon;
pub mod domain;
pub mod dorm;
mod error;
mod event;
mod local;
pub mod monitor;
pub mod notification;
pub mod settings;
pub mod source;
pub mod storage;

pub use domain::{Location, Reading};
pub use dorm::{
    Config, ENDPOINT, SelectionLevel, SelectionOption, client, parse_response,
    parse_selection_options, query, selection_form, selection_options, validate_location_echo,
    validate_room_echo, with_dorm_auth, with_dorm_auth_overrides, with_dorm_directory_auth,
    with_dorm_directory_auth_overrides,
};
pub use error::QueryError;
pub use event::Event;
pub use storage::{alert_due, clear_alert, history, mark_alert, save_event};

#[cfg(test)]
mod tests;
