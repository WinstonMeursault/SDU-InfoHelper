use std::{collections::BTreeMap, fs, path::Path, str::FromStr, sync::LazyLock, time::Duration};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use regex::Regex;
use reqwest::{StatusCode, blocking::Client, redirect::Policy};
use rusqlite::{Connection, OptionalExtension, params};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ENDPOINT: &str = "https://mcard.sdu.edu.cn/charge/feeitem/getThirdData";

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("{0}")]
    Config(&'static str),
    #[error("登录凭据已失效，请在 App 查询后重新导入抓包。")]
    Authentication,
    #[error("连接学校接口超时，未获得有效电量。")]
    Timeout,
    #[error("连接学校接口失败，未获得有效电量。")]
    Network,
    #[error("学校接口返回 HTTP {0}，未获得有效电量。")]
    Http(u16),
    #[error("{0}")]
    Response(&'static str),
}

#[derive(Clone, Deserialize)]
pub struct Config {
    pub schema_version: u32,
    pub url: String,
    pub form: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, QueryError> {
        let text = fs::read_to_string(path)
            .map_err(|_| QueryError::Config("无法读取查询配置，请先用 Python 工具导入抓包。"))?;
        let config: Self = serde_json::from_str(&text)
            .map_err(|_| QueryError::Config("查询配置格式不匹配，请重新导入抓包。"))?;
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
                "缺少 synjones-auth，请重新导入 App 抓包。",
            ));
        }
        Ok(())
    }

    fn auth(&self) -> Option<&str> {
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

#[derive(Clone, Debug, Serialize)]
pub struct Location {
    pub campus: String,
    pub building: String,
    pub floor: String,
    pub room: String,
}

impl Location {
    pub fn label(&self) -> String {
        fn name(value: &str) -> &str {
            value.split_once('&').map_or(value, |(_, name)| name)
        }
        format!(
            "{} / {}栋 / {}层 / {}号",
            name(&self.campus),
            name(&self.building),
            name(&self.floor),
            name(&self.room)
        )
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
    fn number(self) -> usize {
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

#[derive(Debug, PartialEq)]
pub struct Reading {
    pub remaining_kwh: Decimal,
    pub supply_status: Option<String>,
}

static ENERGY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"剩余电量为\s*(-?[0-9]+(?:\.[0-9]+)?)\s*度").expect("valid constant regex")
});
static SUPPLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"供电状态[：:]\s*(.*)").expect("valid constant regex"));

fn check_api_code(payload: &Value) -> Result<(), QueryError> {
    match payload.get("code") {
        Some(Value::Number(code)) if code.as_i64() == Some(401) || code.as_i64() == Some(403) => {
            return Err(QueryError::Authentication);
        }
        Some(Value::String(code)) if code == "401" || code == "403" => {
            return Err(QueryError::Authentication);
        }
        Some(Value::Number(code)) if code.as_i64() == Some(200) => {}
        _ => {
            return Err(QueryError::Response(
                "学校接口返回业务错误，未获得有效电量。",
            ));
        }
    }
    Ok(())
}

pub fn parse_response(payload: &Value) -> Result<Reading, QueryError> {
    check_api_code(payload)?;
    let info = payload
        .pointer("/map/showData/信息")
        .and_then(Value::as_str)
        .ok_or(QueryError::Response("响应中没有电量信息，未记录电量。"))?;
    let energy = ENERGY
        .captures(info)
        .ok_or(QueryError::Response("无法识别剩余电量，未记录电量。"))?;
    let remaining_kwh =
        Decimal::from_str(&energy[1]).map_err(|_| QueryError::Response("电量数值格式不匹配。"))?;
    let supply_status = SUPPLY
        .captures(info)
        .map(|matched| matched[1].trim().to_owned());
    Ok(Reading {
        remaining_kwh,
        supply_status,
    })
}

pub fn client(timeout: Duration) -> Result<Client, QueryError> {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .redirect(Policy::none())
        .no_proxy()
        .build()
        .map_err(|_| QueryError::Network)
}

fn post_form(
    config: &Config,
    client: &Client,
    form: &BTreeMap<String, String>,
) -> Result<Value, QueryError> {
    config.validate()?;
    let mut request = client.post(ENDPOINT).form(form);
    // Send only the headers required by the verified query; config is never logged.
    for (key, value) in &config.headers {
        if [
            "synjones-auth",
            "accept",
            "origin",
            "user-agent",
            "x-requested-with",
        ]
        .iter()
        .any(|allowed| key.eq_ignore_ascii_case(allowed))
        {
            request = request.header(key, value);
        }
    }
    let response = request.send().map_err(|error| {
        if error.is_timeout() {
            QueryError::Timeout
        } else {
            QueryError::Network
        }
    })?;
    let status = response.status();
    if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || status.is_redirection()
    {
        return Err(QueryError::Authentication);
    }
    if status != StatusCode::OK {
        return Err(QueryError::Http(status.as_u16()));
    }
    let payload: Value = response
        .json()
        .map_err(|_| QueryError::Response("接口未返回 JSON，请检查登录和网络。"))?;
    Ok(payload)
}

pub fn validate_room_echo(payload: &Value, expected: &str) -> Result<(), QueryError> {
    let value = payload
        .pointer("/map/data/room")
        .ok_or(QueryError::Response("响应缺少房间编号，无法核对宿舍。"))?;
    let returned = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        _ => return Err(QueryError::Response("响应房间编号格式不匹配。")),
    };
    let code = expected.split_once('&').map_or(expected, |(code, _)| code);
    if returned != expected && returned != code {
        return Err(QueryError::Response("响应房间与请求不一致，未记录电量。"));
    }
    Ok(())
}

pub fn query(config: &Config, client: &Client) -> Result<Reading, QueryError> {
    let payload = post_form(config, client, &config.form)?;
    let reading = parse_response(&payload)?;
    validate_location_echo(&payload, config)?;
    Ok(reading)
}

pub fn validate_location_echo(payload: &Value, config: &Config) -> Result<(), QueryError> {
    config.validate()?;
    validate_room_echo(payload, &config.form["room"])?;
    for (parameter, field) in [
        ("campus", "area"),
        ("building", "building"),
        ("floor", "floor"),
    ] {
        let value = payload
            .get("map")
            .and_then(|map| map.get("data"))
            .and_then(|data| data.get(field))
            .ok_or(QueryError::Response(
                "响应缺少校区、楼栋或楼层信息，无法核对宿舍。",
            ))?;
        let returned = match value {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            _ => return Err(QueryError::Response("响应宿舍层级格式不匹配。")),
        };
        let expected = &config.form[parameter];
        let code = expected
            .split_once('&')
            .map_or(expected.as_str(), |(code, _)| code);
        if returned != expected.as_str() && returned != code {
            return Err(QueryError::Response(
                "响应宿舍层级与请求不一致，未记录电量。",
            ));
        }
    }
    Ok(())
}

pub fn parse_selection_options(payload: &Value) -> Result<Vec<SelectionOption>, QueryError> {
    check_api_code(payload)?;
    let data = payload
        .pointer("/map/data")
        .ok_or(QueryError::Response("响应中没有宿舍目录。"))?;
    serde_json::from_value(data.clone())
        .map_err(|_| QueryError::Response("宿舍目录响应格式不匹配。"))
}

pub fn selection_options(
    config: &Config,
    client: &Client,
    level: SelectionLevel,
    overrides: &BTreeMap<String, String>,
) -> Result<Vec<SelectionOption>, QueryError> {
    let form = selection_form(config, level, overrides)?;
    parse_selection_options(&post_form(config, client, &form)?)
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub checked_at: String,
    pub remaining_kwh: Option<String>,
    pub unit: &'static str,
    pub supply_status: Option<String>,
    pub threshold_kwh: Option<String>,
    pub low_balance: Option<bool>,
    pub token_expires_at_claim: Option<String>,
    pub error: Option<String>,
    pub location: Option<Location>,
}

impl Event {
    pub fn success(reading: Reading, threshold: Option<Decimal>, expiry: Option<String>) -> Self {
        Self {
            checked_at: Utc::now().to_rfc3339(),
            remaining_kwh: Some(reading.remaining_kwh.to_string()),
            unit: "kWh",
            supply_status: reading.supply_status,
            threshold_kwh: threshold.map(|value| value.to_string()),
            low_balance: threshold.map(|value| reading.remaining_kwh <= value),
            token_expires_at_claim: expiry,
            error: None,
            location: None,
        }
    }

    pub fn failure(error: &QueryError) -> Self {
        Self {
            checked_at: Utc::now().to_rfc3339(),
            remaining_kwh: None,
            unit: "kWh",
            supply_status: None,
            threshold_kwh: None,
            low_balance: None,
            token_expires_at_claim: None,
            error: Some(error.to_string()),
            location: None,
        }
    }

    pub fn at_location(mut self, location: Location) -> Self {
        self.location = Some(location);
        self
    }
}

pub fn history(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS readings (
        id INTEGER PRIMARY KEY, checked_at TEXT NOT NULL,
        remaining_kwh TEXT, supply_status TEXT, threshold_kwh TEXT,
        low_balance INTEGER, error TEXT
    ); CREATE TABLE IF NOT EXISTS alert_state (
        topic TEXT PRIMARY KEY, last_sent INTEGER NOT NULL
    );",
    )?;
    let columns: Vec<String> = {
        let mut statement = connection.prepare("PRAGMA table_info(readings)")?;
        statement
            .query_map([], |row| row.get(1))?
            .collect::<Result<_, _>>()?
    };
    for column in ["campus", "building", "floor", "room"] {
        if !columns.iter().any(|existing| existing == column) {
            connection.execute(
                &format!("ALTER TABLE readings ADD COLUMN {column} TEXT"),
                [],
            )?;
        }
    }
    Ok(connection)
}

pub fn save_event(connection: &Connection, event: &Event) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO readings (checked_at, remaining_kwh, supply_status, threshold_kwh, low_balance, error, campus, building, floor, room) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![event.checked_at, event.remaining_kwh, event.supply_status,
            event.threshold_kwh, event.low_balance, event.error,
            event.location.as_ref().map(|location| &location.campus),
            event.location.as_ref().map(|location| &location.building),
            event.location.as_ref().map(|location| &location.floor),
            event.location.as_ref().map(|location| &location.room)],
    )?;
    Ok(())
}

pub fn alert_due(
    connection: &Connection,
    topic: &str,
    now: i64,
    repeat_after: u64,
) -> rusqlite::Result<bool> {
    let last: Option<i64> = connection
        .query_row(
            "SELECT last_sent FROM alert_state WHERE topic = ?1",
            [topic],
            |row| row.get(0),
        )
        .optional()?;
    Ok(last.is_none_or(|last| now.saturating_sub(last) >= repeat_after as i64))
}

pub fn mark_alert(connection: &Connection, topic: &str, now: i64) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO alert_state (topic, last_sent) VALUES (?1, ?2) \
        ON CONFLICT(topic) DO UPDATE SET last_sent = excluded.last_sent",
        params![topic, now],
    )?;
    Ok(())
}

pub fn clear_alert(connection: &Connection, topic: &str) -> rusqlite::Result<()> {
    connection.execute("DELETE FROM alert_state WHERE topic = ?1", [topic])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload(info: &str) -> Value {
        json!({"code": 200, "map": {"showData": {"信息": info}}})
    }

    fn config() -> Config {
        serde_json::from_value(json!({
            "schema_version": 1, "url": ENDPOINT,
            "form": {"feeitemid":"411", "type":"IEC", "level":"4", "campus":"C1&Campus", "building":"B1&Building", "floor":"F1&Floor", "room":"R1&Room"},
            "headers": {"synjones-auth": "test-token"}
        })).unwrap()
    }

    #[test]
    fn directory_forms_include_only_required_parents() {
        let config = config();
        for (level, number) in [
            (SelectionLevel::Campuses, 0),
            (SelectionLevel::Buildings, 1),
            (SelectionLevel::Floors, 2),
            (SelectionLevel::Rooms, 3),
        ] {
            let form = selection_form(&config, level, &BTreeMap::new()).unwrap();
            assert_eq!(form.len(), 3 + number);
            assert_eq!(form["type"], "select");
            assert_eq!(form["level"], number.to_string());
            assert!(!form.contains_key("room"));
        }
        assert_eq!(config.form["type"], "IEC");
    }

    #[test]
    fn changing_building_requires_new_floor_and_room() {
        let config = config();
        let mut overrides = BTreeMap::from([("building".into(), "B2&Other".into())]);
        assert!(config.with_location_overrides(&overrides).is_err());
        overrides.insert("floor".into(), "F2&Other".into());
        overrides.insert("room".into(), "R2&Other".into());
        let changed = config.with_location_overrides(&overrides).unwrap();
        assert_eq!(changed.form["room"], "R2&Other");
        assert_eq!(config.form["room"], "R1&Room");
    }

    #[test]
    fn another_room_on_same_floor_can_be_selected_alone() {
        let config = config();
        let changed = config
            .with_location_overrides(&BTreeMap::from([("room".into(), "R2&Other".into())]))
            .unwrap();
        assert_eq!(changed.form["floor"], config.form["floor"]);
        assert_eq!(changed.form["room"], "R2&Other");
    }

    #[test]
    fn listing_rooms_in_new_building_requires_a_floor() {
        let config = config();
        let mut overrides = BTreeMap::from([("building".into(), "B2&Other".into())]);
        assert!(selection_form(&config, SelectionLevel::Rooms, &overrides).is_err());
        overrides.insert("floor".into(), "F2&Other".into());
        let form = selection_form(&config, SelectionLevel::Rooms, &overrides).unwrap();
        assert_eq!(form["floor"], "F2&Other");
    }

    #[test]
    fn directory_values_are_preserved_and_malformed_data_is_rejected() {
        let options = parse_selection_options(
            &json!({"code":200,"map":{"data":[{"name":"202","value":"R2&202"}]}}),
        )
        .unwrap();
        assert_eq!(
            options,
            vec![SelectionOption {
                name: "202".into(),
                value: "R2&202".into()
            }]
        );
        assert!(parse_selection_options(&json!({"code":200,"map":{"data":{}}})).is_err());
        assert!(matches!(
            parse_selection_options(&json!({"code":401})),
            Err(QueryError::Authentication)
        ));
    }

    #[test]
    fn a_valid_energy_response_for_the_wrong_room_is_rejected() {
        let mut response = payload("剩余电量为12.34度");
        response["map"]["data"] = json!({"room":"R2"});
        assert!(validate_room_echo(&response, "R2&202").is_ok());
        assert!(validate_room_echo(&response, "R1&101").is_err());
        assert!(validate_room_echo(&payload("剩余电量为12.34度"), "R2&202").is_err());
    }

    #[test]
    fn correct_room_with_wrong_parent_location_is_rejected() {
        let config = config();
        let mut response = payload("剩余电量为12.34度");
        response["map"]["data"] = json!({"area":"C1","building":"B1","floor":"F1","room":"R1"});
        assert!(validate_location_echo(&response, &config).is_ok());
        response["map"]["data"]["floor"] = json!("F2");
        assert!(validate_location_echo(&response, &config).is_err());
    }

    #[test]
    fn legacy_history_is_migrated_and_new_rooms_are_recorded_separately() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch("CREATE TABLE readings (id INTEGER PRIMARY KEY, checked_at TEXT NOT NULL, remaining_kwh TEXT, supply_status TEXT, threshold_kwh TEXT, low_balance INTEGER, error TEXT); INSERT INTO readings (checked_at,remaining_kwh) VALUES ('legacy','35.67');").unwrap();
        }
        let connection = history(&path).unwrap();
        let config = config();
        for room in ["R1&101", "R2&202"] {
            let changed = config
                .with_location_overrides(&BTreeMap::from([("room".into(), room.into())]))
                .unwrap();
            let event = Event::success(
                parse_response(&payload("剩余电量为12.34度")).unwrap(),
                None,
                None,
            )
            .at_location(changed.location().unwrap());
            save_event(&connection, &event).unwrap();
        }
        let mut statement = connection
            .prepare("SELECT room FROM readings ORDER BY id")
            .unwrap();
        let rooms: Vec<Option<String>> = statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rooms,
            vec![None, Some("R1&101".into()), Some("R2&202".into())]
        );
    }

    #[test]
    fn valid_energy_survives_failed_supply_status() {
        let reading = parse_response(&payload("剩余电量为35.67度，供电状态：查询失败   ")).unwrap();
        assert_eq!(reading.remaining_kwh, Decimal::from_str("35.67").unwrap());
        assert_eq!(reading.supply_status.as_deref(), Some("查询失败"));
    }

    #[test]
    fn unavailable_energy_is_not_zero() {
        assert!(parse_response(&payload("剩余电量查询失败")).is_err());
        assert!(Event::failure(&QueryError::Timeout).remaining_kwh.is_none());
    }

    #[test]
    fn zero_and_negative_readings_are_valid() {
        for value in ["0", "-1.23"] {
            let reading = parse_response(&payload(&format!("剩余电量为{value}度"))).unwrap();
            assert_eq!(reading.remaining_kwh, Decimal::from_str(value).unwrap());
        }
    }

    #[test]
    fn expired_login_is_distinct_from_parse_failure() {
        assert!(matches!(
            parse_response(&json!({"code": 401})),
            Err(QueryError::Authentication)
        ));
        assert!(matches!(
            parse_response(&json!({"code": "403"})),
            Err(QueryError::Authentication)
        ));
    }

    #[test]
    fn threshold_is_inclusive_and_exact() {
        let reading = Reading {
            remaining_kwh: Decimal::from_str("10.00").unwrap(),
            supply_status: None,
        };
        let event = Event::success(reading, Some(Decimal::from(10)), None);
        assert_eq!(event.low_balance, Some(true));
    }

    #[test]
    fn failed_event_is_stored_as_null_energy() {
        let directory = tempfile::tempdir().unwrap();
        let connection = history(&directory.path().join("history.sqlite3")).unwrap();
        save_event(&connection, &Event::failure(&QueryError::Authentication)).unwrap();
        let energy: Option<String> = connection
            .query_row("SELECT remaining_kwh FROM readings", [], |row| row.get(0))
            .unwrap();
        assert!(energy.is_none());
    }

    #[test]
    fn cooldown_survives_restart_and_resets_after_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        {
            let connection = history(&path).unwrap();
            assert!(alert_due(&connection, "low", 100, 60).unwrap());
            mark_alert(&connection, "low", 100).unwrap();
        }
        let connection = history(&path).unwrap();
        assert!(!alert_due(&connection, "low", 159, 60).unwrap());
        assert!(alert_due(&connection, "low", 160, 60).unwrap());
        clear_alert(&connection, "low").unwrap();
        assert!(alert_due(&connection, "low", 159, 60).unwrap());
    }

    #[test]
    fn payment_actions_and_other_hosts_are_rejected() {
        let mut config: Config = serde_json::from_value(json!({
            "schema_version": 1, "url": ENDPOINT,
            "form": {"feeitemid":"411", "type":"IEC", "level":"4", "campus":"test", "building":"test", "floor":"test", "room":"test"},
            "headers": {"synjones-auth": "test"}
        })).unwrap();
        assert!(config.validate().is_ok());
        config.form.insert("type".into(), "pay".into());
        assert!(config.validate().is_err());
        config.form.insert("type".into(), "IEC".into());
        config.url = "https://example.invalid".into();
        assert!(config.validate().is_err());
    }
}
