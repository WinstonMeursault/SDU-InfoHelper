//! Resolve configured names using verified directory entries.
use super::{Config, ENDPOINT, SelectionLevel, selection_options};
use crate::{
    QueryError,
    control::{OperationControl, OperationError},
    settings::{DormTarget, SelectorValue},
};
use reqwest::blocking::Client;
use std::collections::BTreeMap;

pub(crate) fn resolve(
    target: &DormTarget,
    token: &str,
    client: &Client,
    depth: usize,
) -> Result<Config, QueryError> {
    resolve_controlled(
        target,
        token,
        client,
        depth,
        &OperationControl::uninterrupted(std::time::Duration::ZERO),
    )
    .map_err(OperationError::into_query)
}

pub(crate) fn resolve_controlled(
    target: &DormTarget,
    token: &str,
    client: &Client,
    depth: usize,
    control: &OperationControl<'_>,
) -> Result<Config, OperationError> {
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
    let room = target.room.as_ref().map(SelectorValue::text);
    let derived_floor = room
        .as_deref()
        .and_then(|r| r.parse::<u16>().ok())
        .filter(|r| *r >= 100)
        .map(|r| (r / 100).to_string());
    let targets = [
        (
            "campus",
            SelectionLevel::Campuses,
            target
                .campus
                .as_ref()
                .map(SelectorValue::text)
                .or(Some("主校区".into())),
        ),
        (
            "building",
            SelectionLevel::Buildings,
            target.building.as_ref().map(SelectorValue::text),
        ),
        (
            "floor",
            SelectionLevel::Floors,
            target
                .floor
                .as_ref()
                .map(SelectorValue::text)
                .or(derived_floor),
        ),
        ("room", SelectionLevel::Rooms, room),
    ];
    for (key, level, value) in targets.into_iter().take(depth) {
        control.check()?;
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
    fn room_shorthand_requires_unique_directory_match() {
        let options = vec![crate::SelectionOption {
            name: "05".into(),
            value: "server-id&05".into(),
        }];
        assert_eq!(select_name(&options, "405", true).unwrap(), "server-id&05");
        assert!(select_name(&options, "406", true).is_err());
    }
}
