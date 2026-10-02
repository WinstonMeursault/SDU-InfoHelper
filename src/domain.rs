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

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub remaining_kwh: Decimal,
    pub supply_status: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryStatus {
    Pending,
    Accepted,
    Blocked,
    Exhausted,
    Cancelled,
}

impl DeliveryStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Blocked => "blocked",
            Self::Exhausted => "exhausted",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "blocked" => Some(Self::Blocked),
            "exhausted" => Some(Self::Exhausted),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}
