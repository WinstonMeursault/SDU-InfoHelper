//! Unified configuration. Credentials never implement Debug and are never logged.
use crate::{Config, ENDPOINT, QueryError, SelectionLevel, selection_options};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub mod monitoring;
pub use monitoring::{DaemonSettings, NotificationSettings};

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub device_id: Option<String>,
}

impl Credentials {
    pub fn validate(&self) -> Result<(), QueryError> {
        if self.username.trim().is_empty()
            || self.password.is_empty()
            || self.username == "你的学号"
            || self.password == "你的统一身份认证密码"
        {
            return Err(QueryError::AuthFlow(
                "请先在本机 config.yaml 填写统一身份认证账号和密码。",
            ));
        }
        Ok(())
    }
}

// Parent repository uses numeric rooms; API directories use strings such as id&name.
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SelectorValue {
    Text(String),
    Number(u16),
}
impl SelectorValue {
    pub fn text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Number(n) => n.to_string(),
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct DormTarget {
    pub campus: Option<SelectorValue>,
    pub building: Option<SelectorValue>,
    pub floor: Option<SelectorValue>,
    pub room: Option<SelectorValue>,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub struct AirconTarget {
    pub building: Option<u16>,
    pub floor: Option<u16>,
    pub room: Option<u16>,
}
impl AirconTarget {
    pub fn selected(self) -> Result<(u16, u16, u16), QueryError> {
        let building = self.building.filter(|x| *x > 0);
        let room = self.room.filter(|x| *x > 0);
        let floor = self.floor.or(self.room.map(|x| x / 100)).filter(|x| *x > 0);
        match (building, floor, room) {
            (Some(b), Some(f), Some(r)) => Ok((b, f, r)),
            _ => Err(QueryError::Config(
                "请填写 aircon.building、aircon.room 和有效楼层。",
            )),
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct AuthSettings {
    pub cache: Option<PathBuf>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Settings {
    pub cas: Option<Credentials>,
    #[serde(default)]
    pub auth: AuthSettings,
    #[serde(default)]
    pub dorm_electricity: DormTarget,
    #[serde(default)]
    pub aircon: AirconTarget,
    #[serde(default)]
    pub daemon: DaemonSettings,
    #[serde(default)]
    pub notifications: NotificationSettings,
}

impl Settings {
    pub(crate) fn cas_username(&self) -> Option<&str> {
        self.cas
            .as_ref()
            .map(|credentials| credentials.username.as_str())
            .filter(|username| !username.trim().is_empty())
    }

    pub(crate) fn with_dorm_overrides(
        &self,
        overrides: &BTreeMap<String, String>,
        depth: usize,
    ) -> Result<Self, QueryError> {
        let mut settings = self.clone();
        let target = &mut settings.dorm_electricity;
        let derived_floor = target
            .room
            .as_ref()
            .and_then(|room| room.text().parse::<u16>().ok())
            .filter(|room| *room >= 100)
            .map(|room| (room / 100).to_string());
        let mut fields = [
            ("campus", &mut target.campus),
            ("building", &mut target.building),
            ("floor", &mut target.floor),
            ("room", &mut target.room),
        ];
        let fields = &mut fields[..depth];
        if overrides.iter().any(|(key, value)| {
            value.trim().is_empty() || !fields.iter().any(|(name, _)| *name == key)
        }) {
            return Err(QueryError::Config("宿舍覆盖参数不匹配该查询层级。"));
        }
        for (index, (key, original)) in fields.iter().enumerate() {
            let original = original.as_ref().map(SelectorValue::text);
            let original = original.or_else(|| match *key {
                "campus" => Some("主校区".into()),
                "floor" => derived_floor.clone(),
                _ => None,
            });
            if overrides.get(*key).is_some_and(|value| {
                original
                    .as_deref()
                    .is_none_or(|original| !same_selector(original, value))
            }) && fields[index + 1..]
                .iter()
                .any(|(child, _)| !overrides.contains_key(*child))
            {
                return Err(QueryError::Config(
                    "更换上级目录时，请同时指定查询所需的下级宿舍参数。",
                ));
            }
        }
        for (key, value) in fields {
            if let Some(override_value) = overrides.get(*key) {
                **value = Some(SelectorValue::Text(override_value.clone()));
            }
        }
        Ok(settings)
    }

    pub fn load(path: &Path) -> Result<Self, QueryError> {
        let text = fs::read_to_string(path).map_err(|_| {
            QueryError::Config("无法读取 config.yaml，请复制 config.example.yaml 后在本机填写。")
        })?;
        let settings: Self = serde_yaml::from_str(&text)
            .map_err(|_| QueryError::Config("config.yaml 格式不匹配，请检查配置结构。"))?;
        settings.daemon.validate()?;
        settings.notifications.validate()?;
        Ok(settings)
    }
    pub fn cache_path(&self, config_path: &Path) -> PathBuf {
        let base = config_path.parent().unwrap_or(Path::new("."));
        let path = self
            .auth
            .cache
            .as_deref()
            .unwrap_or(Path::new(".local/electricity/auth.json"));
        if path.is_absolute() {
            path.to_owned()
        } else {
            base.join(path)
        }
    }

    /// Resolve human directory names using the server, without guessing ID encoding.
    /// A room such as 405 means floor 4 / room 05 only if that directory entry exists.
    pub fn dorm_request(
        &self,
        token: &str,
        client: &reqwest::blocking::Client,
    ) -> Result<Config, QueryError> {
        self.dorm_request_depth(token, client, 4)
    }

    pub(crate) fn dorm_request_depth(
        &self,
        token: &str,
        client: &reqwest::blocking::Client,
        depth: usize,
    ) -> Result<Config, QueryError> {
        let mut request = Config {
            schema_version: 1,
            url: ENDPOINT.into(),
            form: BTreeMap::from([
                ("feeitemid".into(), "411".into()),
                ("type".into(), "IEC".into()),
                ("level".into(), "4".into()),
                ("campus".into(), "_".into()),
                ("building".into(), "_".into()),
                ("floor".into(), "_".into()),
                ("room".into(), "_".into()),
            ]),
            headers: BTreeMap::from([("synjones-auth".into(), format!("bearer {token}"))]),
        };
        let room = self.dorm_electricity.room.as_ref().map(SelectorValue::text);
        let derived_floor = room
            .as_deref()
            .and_then(|r| r.parse::<u16>().ok())
            .filter(|r| *r >= 100)
            .map(|r| (r / 100).to_string());
        let targets = [
            (
                "campus",
                SelectionLevel::Campuses,
                self.dorm_electricity
                    .campus
                    .as_ref()
                    .map(SelectorValue::text)
                    .or(Some("主校区".into())),
            ),
            (
                "building",
                SelectionLevel::Buildings,
                self.dorm_electricity
                    .building
                    .as_ref()
                    .map(SelectorValue::text),
            ),
            (
                "floor",
                SelectionLevel::Floors,
                self.dorm_electricity
                    .floor
                    .as_ref()
                    .map(SelectorValue::text)
                    .or(derived_floor),
            ),
            ("room", SelectionLevel::Rooms, room),
        ];
        for (key, level, value) in targets.into_iter().take(depth) {
            if let Some(value) = value.filter(|v| !v.is_empty()) {
                // Exact values previously obtained from the directory can be reused.
                if value.contains('&') {
                    request.form.insert(key.into(), value);
                    continue;
                }
                let options = selection_options(&request, client, level, &BTreeMap::new())?;
                let found = select_name(&options, &value, key == "room")?;
                request.form.insert(key.into(), found);
            } else {
                // Lists require only their ancestors. query validates the entire target.
                request.form.insert(key.into(), "_".into());
            }
        }
        Ok(request)
    }
}

fn same_selector(original: &str, replacement: &str) -> bool {
    if original == replacement {
        return true;
    }
    match (original.split_once('&'), replacement.split_once('&')) {
        (Some((original_id, _)), Some((replacement_id, _))) => original_id == replacement_id,
        (Some((_, name)), None) => name == replacement,
        (None, Some((_, name))) => original == name,
        (None, None) => false,
    }
}

fn select_name(
    options: &[crate::SelectionOption],
    value: &str,
    room: bool,
) -> Result<String, QueryError> {
    let matches: Vec<_> = options
        .iter()
        .filter(|o| o.name == value || o.value == value)
        .collect();
    if matches.len() == 1 {
        return Ok(matches[0].value.clone());
    }
    if matches.is_empty()
        && room
        && let Ok(number) = value.parse::<u16>()
    {
        // Allow numeric shorthand only when the returned room names confirm it.
        let matches: Vec<_> = options
            .iter()
            .filter(|o| o.name.parse::<u16>().ok() == Some(number % 100))
            .collect();
        if matches.len() == 1 {
            return Ok(matches[0].value.clone());
        }
    }
    Err(QueryError::Config(
        "目录中未找到唯一匹配，请使用 list 返回的完整参数值。",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parent_config_is_compatible() {
        let s:Settings=serde_yaml::from_str("cas:\n  username: test\n  password: test\naircon:\n  building: 3\n  room: 405\ndorm_electricity:\n  building: 8\n  room: 414\noutput_html: output/a.html\n").unwrap();
        assert_eq!(s.aircon.selected().unwrap(), (3, 4, 405));
        assert_eq!(s.dorm_electricity.room.unwrap().text(), "414");
    }
    #[test]
    fn room_shorthand_requires_unique_directory_match() {
        let options = vec![crate::SelectionOption {
            name: "05".into(),
            value: "server-id&05".into(),
        }];
        assert_eq!(select_name(&options, "405", true).unwrap(), "server-id&05");
        assert!(select_name(&options, "406", true).is_err());
    }

    #[test]
    fn merged_targets_validate_required_descendants_and_query_depth() {
        let settings: Settings = serde_yaml::from_str(
            "dorm_electricity:\n  campus: C&Campus\n  building: removed-building\n  floor: removed-floor\n  room: removed-room\n",
        ).unwrap();
        let mut overrides = BTreeMap::from([("building".into(), "B&Building".into())]);
        assert!(settings.with_dorm_overrides(&overrides, 4).is_err());
        assert!(settings.with_dorm_overrides(&overrides, 2).is_ok());
        overrides.insert("floor".into(), "F&Floor".into());
        assert!(settings.with_dorm_overrides(&overrides, 3).is_ok());
        assert!(settings.with_dorm_overrides(&overrides, 4).is_err());
        overrides.insert("room".into(), "R&Room".into());
        let merged = settings.with_dorm_overrides(&overrides, 4).unwrap();
        assert_eq!(
            merged.dorm_electricity.building.unwrap().text(),
            "B&Building"
        );
        assert_eq!(
            settings.dorm_electricity.building.as_ref().unwrap().text(),
            "removed-building"
        );
        assert!(settings.with_dorm_overrides(&overrides, 3).is_err());
        assert!(
            settings
                .with_dorm_overrides(&BTreeMap::from([("room".into(), " ".into())]), 4)
                .is_err()
        );
    }

    #[test]
    fn equivalent_directory_values_keep_the_existing_descendants() {
        let settings: Settings = serde_yaml::from_str(
            "dorm_electricity:\n  campus: 主校区\n  building: B&8\n  room: 405\n",
        )
        .unwrap();
        for (key, value) in [("campus", "C&主校区"), ("building", "8"), ("floor", "F&4")] {
            assert!(
                settings
                    .with_dorm_overrides(&BTreeMap::from([(key.into(), value.into())]), 4)
                    .is_ok()
            );
        }
        assert!(!same_selector("B1&8", "B2&8"));
        assert!(same_selector("B1&Old name", "B1&New name"));
    }
}
