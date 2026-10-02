//! Portable worker and user-service management facade.
mod instance;
mod log;
mod notification;
mod options;
pub mod platform;
mod signals;
mod state;
mod status;
mod worker;

use instance::private_file;
pub use instance::{InstanceLock, Paths};
pub use log::Log;
pub use notification::test_notification;
pub use options::RunOptions;
pub use state::{LastSuccess, RuntimeState};
pub use status::{LocalStatus, local_status};
pub use worker::run;

#[cfg(test)]
mod tests;
