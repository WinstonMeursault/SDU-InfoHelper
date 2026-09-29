//! The upstream air-conditioning meter is a separate HTML service.
use crate::{
    QueryError,
    auth::{self, LoginOptions},
    settings::AirconTarget,
};
use reqwest::{Url, blocking::Client};
use rust_decimal::Decimal;
use scraper::{Html, Selector};
use serde::Serialize;
use std::{collections::HashMap, str::FromStr, time::Duration};

#[derive(Serialize, Debug, PartialEq)]
pub struct AirconReading {
    pub building: u16,
    pub floor: u16,
    pub room: u16,
    pub remaining_kwh: String,
}

pub fn parse_reading(page: &str) -> Result<AirconReading, QueryError> {
    let document = Html::parse_document(page);
    let rows = Selector::parse("tr").unwrap();
    let cells = Selector::parse("td").unwrap();
    let mut fields = HashMap::new();
    for row in document.select(&rows) {
        let mut values = row.select(&cells).map(|cell| {
            cell.text()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        });
        if let (Some(label), Some(value)) = (values.next(), values.next()) {
            fields.insert(label.trim_end_matches([':', '：']).trim().to_owned(), value);
        }
    }
    let field = |key: &str| {
        fields
            .get(key)
            .map(String::as_str)
            .ok_or(QueryError::Response("空调电量页面缺少必要字段。"))
    };
    let number = |key: &str| {
        field(key)?
            .parse::<u16>()
            .map_err(|_| QueryError::Response("空调页面房间字段格式错误。"))
    };
    let remaining = Decimal::from_str(field("剩余电量")?)
        .map_err(|_| QueryError::Response("空调剩余电量不是有效数字。"))?;
    Ok(AirconReading {
        building: number("公寓")?,
        floor: number("楼层")?,
        room: number("房间")?,
        remaining_kwh: remaining.to_string(),
    })
}

pub fn query(client: &Client, target: AirconTarget) -> Result<AirconReading, QueryError> {
    let (building, floor, room) = target.selected()?;
    let mut url = Url::parse("https://gyktgd.wh.sdu.edu.cn/dianbiao/chongzhi.jsp").unwrap();
    url.query_pairs_mut()
        .append_pair("gongyu", &building.to_string())
        .append_pair("sushe", &room.to_string())
        .append_pair("floor", &floor.to_string());
    let response = client.get(url).send().map_err(|e| {
        if e.is_timeout() {
            QueryError::Timeout
        } else {
            QueryError::Network
        }
    })?;
    if response.url().host_str() != Some("gyktgd.wh.sdu.edu.cn") {
        return Err(QueryError::Authentication);
    }
    if !response.status().is_success() {
        return Err(QueryError::Http(response.status().as_u16()));
    }
    let reading = parse_reading(&response.text().map_err(|_| QueryError::Network)?)?;
    if (reading.building, reading.floor, reading.room) != (building, floor, room) {
        return Err(QueryError::Response(
            "空调页面的公寓、楼层或房间与请求不一致。",
        ));
    }
    Ok(reading)
}
pub fn query_config(
    path: &std::path::Path,
    timeout: Duration,
    options: LoginOptions,
) -> Result<AirconReading, QueryError> {
    let (client, target) = auth::aircon_session(path, timeout, options)?;
    query(&client, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upstream_html_format_and_decimal_precision() {
        let html = "<table><tr><td>公寓: </td><td>3</td></tr><tr><td>楼层：</td><td>4</td></tr><tr><td>房间:</td><td>405</td></tr><tr><td>剩余电量:</td><td>7.01</td></tr></table>";
        let reading = parse_reading(html).unwrap();
        assert_eq!((reading.building, reading.floor, reading.room), (3, 4, 405));
        assert_eq!(reading.remaining_kwh, "7.01");
        assert!(parse_reading(&html.replace("7.01", "NaN")).is_err());
    }
}
