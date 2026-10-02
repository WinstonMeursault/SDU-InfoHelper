//! Typed electricity readings and resolved dorm locations.
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Debug, PartialEq)]
pub struct Reading {
    pub remaining_kwh: Decimal,
    pub supply_status: Option<String>,
}
