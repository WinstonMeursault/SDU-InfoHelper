//! Verified dorm requests and legacy request.json compatibility.
use super::ENDPOINT;
use crate::{Location, QueryError};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path};

#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    pub schema_version: u32,
    pub url: String,
    pub form: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, QueryError> {
        let text =
            fs::read_to_string(path).map_err(|_| QueryError::Config("无法读取查询配置。"))?;
        let config: Self =
            serde_json::from_str(&text).map_err(|_| QueryError::Config("查询配置格式不匹配。"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), QueryError> {
        let fields = [
            "building",
            "campus",
            "feeitemid",
            "floor",
            "level",
            "room",
            "type",
        ];
        if self.schema_version != 1 || self.url != ENDPOINT {
            return Err(QueryError::Config("仅支持已验证的电费查询接口。"));
        }
        if self.form.len() != fields.len()
            || fields
                .iter()
                .any(|key| self.form.get(*key).is_none_or(String::is_empty))
            || self.form.get("feeitemid").map(String::as_str) != Some("411")
            || self.form.get("type").map(String::as_str) != Some("IEC")
            || self.form.get("level").map(String::as_str) != Some("4")
        {
            return Err(QueryError::Config("配置不是完整的威海电费余额查询。"));
        }
        if self.auth().is_none_or(str::is_empty) {
            return Err(QueryError::Config(
                "缺少 synjones-auth，请运行 auth login 或 auth import。",
            ));
        }
        Ok(())
    }

    pub fn auth(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("synjones-auth"))
            .map(|(_, value)| value.as_str())
    }

    pub fn expiry_claim(&self) -> Option<String> {
        // Metadata only: the server verifies authentication, not this unsigned decode.
        let segment = self.auth()?.split_whitespace().last()?.split('.').nth(1)?;
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).ok()?).ok()?;
        DateTime::<Utc>::from_timestamp(claims.get("exp")?.as_i64()?, 0)
            .map(|timestamp| timestamp.to_rfc3339())
    }

    pub fn location(&self) -> Result<Location, QueryError> {
        self.validate()?;
        Ok(Location {
            campus: self.form["campus"].clone(),
            building: self.form["building"].clone(),
            floor: self.form["floor"].clone(),
            room: self.form["room"].clone(),
        })
    }

    pub fn with_location_overrides(
        &self,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Self, QueryError> {
        self.validate()?;
        let fields = ["campus", "building", "floor", "room"];
        for (key, value) in overrides {
            if value.is_empty() || !fields.contains(&key.as_str()) {
                return Err(QueryError::Config("宿舍覆盖参数不匹配。"));
            }
        }
        for (index, key) in fields.iter().enumerate() {
            if overrides
                .get(*key)
                .is_some_and(|value| self.form.get(*key) != Some(value))
                && fields[index + 1..]
                    .iter()
                    .any(|child| !overrides.contains_key(*child))
            {
                return Err(QueryError::Config(
                    "更换校区、楼栋或楼层时，需同时指定下级宿舍参数。",
                ));
            }
        }
        let mut config = self.clone();
        config.form.extend(overrides.clone());
        config.validate()?;
        Ok(config)
    }
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum SelectionLevel {
    Campuses,
    Buildings,
    Floors,
    Rooms,
}

impl SelectionLevel {
    pub(crate) fn number(self) -> usize {
        match self {
            Self::Campuses => 0,
            Self::Buildings => 1,
            Self::Floors => 2,
            Self::Rooms => 3,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
pub struct SelectionOption {
    pub name: String,
    pub value: String,
}

pub fn selection_form(
    config: &Config,
    level: SelectionLevel,
    overrides: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, QueryError> {
    config.validate()?;
    let all_parents = ["campus", "building", "floor"];
    let parents = &all_parents[..level.number()];
    for (key, value) in overrides {
        if value.is_empty() || !parents.contains(&key.as_str()) {
            return Err(QueryError::Config("目录查询参数不匹配该层级。"));
        }
    }
    for (index, key) in parents.iter().enumerate() {
        if overrides
            .get(*key)
            .is_some_and(|value| config.form.get(*key) != Some(value))
            && parents[index + 1..]
                .iter()
                .any(|child| !overrides.contains_key(*child))
        {
            return Err(QueryError::Config(
                "更换上级目录时，请同时指定查询所需的下级目录参数。",
            ));
        }
    }
    let mut form = BTreeMap::from([
        ("feeitemid".into(), "411".into()),
        ("type".into(), "select".into()),
        ("level".into(), level.number().to_string()),
    ]);
    for key in parents {
        form.insert(
            (*key).into(),
            overrides.get(*key).unwrap_or(&config.form[*key]).clone(),
        );
    }
    Ok(form)
}
