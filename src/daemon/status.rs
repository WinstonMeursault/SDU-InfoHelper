//! Merge instance files with native service status without mutating either.
use super::{Paths, RuntimeState, platform, state::WorkerStatus};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct LocalStatus {
    pub instance: String,
    pub running: bool,
    pub data_directory: PathBuf,
    pub runtime: Option<RuntimeState>,
    pub service: platform::ServiceStatus,
}

pub fn local_status(config: &Path) -> Result<LocalStatus, String> {
    let paths = Paths::new(config)?;
    let running = paths.running()?;
    let mut runtime = paths.state()?;
    if running {
        let owner = paths.owner()?;
        if runtime.as_ref().is_some_and(|state| owner != state.run_id) {
            runtime = None;
        }
    }
    if let (false, Some(state)) = (running, &mut runtime) {
        if !matches!(state.status.as_str(), "stopped" | "failed") {
            state.status = WorkerStatus::Terminated.as_str().into();
        }
        state.next_check_at = None;
    }
    Ok(LocalStatus {
        instance: paths.instance,
        running,
        data_directory: paths.directory,
        runtime,
        service: platform::status(config)?,
    })
}
