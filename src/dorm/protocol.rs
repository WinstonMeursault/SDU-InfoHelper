//! School JSON protocol, response validation and HTTP requests.
use super::{Config, ENDPOINT, SelectionLevel, SelectionOption, selection_form};
use crate::{QueryError, Reading};
use regex::Regex;
use reqwest::{StatusCode, blocking::Client, redirect::Policy};
use rust_decimal::Decimal;
use serde_json::Value;
use std::{collections::BTreeMap, str::FromStr, sync::LazyLock, time::Duration};

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
        .use_rustls_tls()
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
