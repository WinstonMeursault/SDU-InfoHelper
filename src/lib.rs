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

#[derive(Deserialize)]
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

pub fn parse_response(payload: &Value) -> Result<Reading, QueryError> {
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

pub fn query(config: &Config, client: &Client) -> Result<Reading, QueryError> {
    config.validate()?;
    let mut request = client.post(ENDPOINT).form(&config.form);
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
    parse_response(&payload)
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
        }
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
    Ok(connection)
}

pub fn save_event(connection: &Connection, event: &Event) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO readings (checked_at, remaining_kwh, supply_status, threshold_kwh, low_balance, error) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![event.checked_at, event.remaining_kwh, event.supply_status,
            event.threshold_kwh, event.low_balance, event.error],
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
