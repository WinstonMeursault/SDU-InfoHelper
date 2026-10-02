//! Bounded private log files owned by an instance.
use super::{Paths, instance::private_file};
use chrono::Utc;
use std::{fs, io::Write};

pub struct Log {
    pub(super) paths: Paths,
    pub(super) max_bytes: u64,
}

impl Log {
    pub fn new(paths: &Paths) -> Self {
        Self {
            paths: paths.clone(),
            max_bytes: 5 * 1024 * 1024,
        }
    }
    pub fn write(&self, message: &str) -> Result<(), String> {
        let line = format!(
            "[{}] {}\n",
            Utc::now().to_rfc3339(),
            message.replace(['\n', '\r'], " ")
        );
        let path = self.paths.file("daemon.log");
        if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) + line.len() as u64 > self.max_bytes {
            for index in (1..=3).rev() {
                let target = self.paths.file(&format!("daemon.log.{index}"));
                let source = if index == 1 {
                    path.clone()
                } else {
                    self.paths.file(&format!("daemon.log.{}", index - 1))
                };
                if target.exists() {
                    fs::remove_file(&target).map_err(|_| "无法轮转监控日志。")?;
                }
                if source.exists() {
                    fs::rename(source, target).map_err(|_| "无法轮转监控日志。")?;
                }
            }
        }
        let mut file = private_file(&path)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::End(0))
            .and_then(|_| file.write_all(line.as_bytes()))
            .map_err(|_| "无法写入监控日志。".into())
    }
}
