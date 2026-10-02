//! Resolve runtime overrides without depending on service registration.
use crate::settings::{DaemonSettings, Settings};
use clap::Args;
use rust_decimal::Decimal;
use std::path::{Path, PathBuf};

#[derive(Args, Clone, Default)]
pub struct RunOptions {
    #[arg(long)]
    pub threshold: Option<Decimal>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    pub interval: Option<u64>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(60..=31_536_000))]
    pub repeat_after: Option<u64>,
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub timeout: Option<u64>,
    #[arg(long)]
    pub history: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub managed: bool,
    #[arg(long, hide = true)]
    pub run_id: Option<String>,
}

impl RunOptions {
    pub fn resolve(&self, settings: &Settings, config: &Path) -> Result<DaemonSettings, String> {
        let mut daemon = settings.daemon.clone();
        if let Some(value) = self.threshold {
            daemon.threshold_kwh = value.normalize();
        }
        if let Some(value) = self.interval {
            daemon.interval_seconds = value;
        }
        if let Some(value) = self.repeat_after {
            daemon.repeat_after_seconds = value;
        }
        if let Some(value) = self.timeout {
            daemon.query_timeout_seconds = value;
        }
        let history_override = self.history.clone().or_else(|| {
            (!self.managed)
                .then(|| std::env::var_os("SDU_INFOHELPER_HISTORY").map(PathBuf::from))
                .flatten()
        });
        daemon.history = match history_override {
            Some(path) if path.is_absolute() => path,
            Some(path) => std::env::current_dir()
                .map_err(|_| "无法读取工作目录。")?
                .join(path),
            None if daemon.history.is_absolute() => daemon.history,
            None => config
                .parent()
                .ok_or("配置目录不可用。")?
                .join(daemon.history),
        };
        daemon.validate().map_err(|e| e.to_string())?;
        Ok(daemon)
    }
}
