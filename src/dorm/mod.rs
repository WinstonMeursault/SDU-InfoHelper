//! Dorm electricity protocol and application service.
pub(crate) mod directory;
mod protocol;
mod request;
mod service;

pub const ENDPOINT: &str = "https://mcard.sdu.edu.cn/charge/feeitem/getThirdData";

pub use protocol::{
    client, parse_response, parse_selection_options, query, selection_options,
    validate_location_echo, validate_room_echo,
};
pub use request::{Config, SelectionLevel, SelectionOption, selection_form};
pub(crate) use service::with_dorm_auth_controlled;
pub use service::{
    with_dorm_auth, with_dorm_auth_overrides, with_dorm_directory_auth,
    with_dorm_directory_auth_overrides,
};
