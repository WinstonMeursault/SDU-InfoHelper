//! Stable trusted-device identity stored with local CAS credentials.
use super::secure_write;
use crate::{
    QueryError,
    settings::{Credentials, Settings},
};
use rand::{RngCore, rngs::OsRng};
use std::{fs, path::Path};

pub(super) fn credentials(config_path: &Path) -> Result<Credentials, QueryError> {
    // Re-read under the authentication lock: another process may have initialized
    // the device ID while this caller was waiting. Replacing it would lose trust.
    let mut credentials = Settings::read(config_path)?
        .cas
        .ok_or(QueryError::AuthFlow(
            "请在本机 config.yaml 配置 cas 账号密码。",
        ))?;
    credentials.validate()?;
    if let Some(id) = credentials.device_id.as_ref() {
        if id.len() < 32 || hex::decode(id).is_err() {
            return Err(QueryError::Config(
                "cas.device_id 必须为至少 32 位十六进制字符串。",
            ));
        }
    } else {
        let mut bytes = [0u8; 16];
        OsRng.fill_bytes(&mut bytes);
        let id = hex::encode(bytes);
        let text = fs::read_to_string(config_path)
            .map_err(|_| QueryError::Config("无法读取设备配置。"))?;
        let mut yaml: serde_yaml::Value = serde_yaml::from_str(&text)
            .map_err(|_| QueryError::Config("设备配置 YAML 格式错误。"))?;
        yaml["cas"]["device_id"] = serde_yaml::Value::String(id.clone());
        secure_write(
            config_path,
            serde_yaml::to_string(&yaml)
                .map_err(|_| QueryError::Config("无法保存设备配置。"))?
                .as_bytes(),
        )?;
        credentials.device_id = Some(id);
    }
    Ok(credentials)
}
