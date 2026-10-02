//! Air-conditioning source has its own typed target and cookie session.
use super::{ElectricitySource, ReadContext, SourceError, SourceReading};
use crate::{
    aircon,
    auth::{self, LoginOptions},
};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AirconLocation {
    pub building: u16,
    pub floor: u16,
    pub room: u16,
}

pub struct AirconSource<'a> {
    config: &'a Path,
    options: LoginOptions,
}

impl<'a> AirconSource<'a> {
    pub fn new(config: &'a Path, options: LoginOptions) -> Self {
        Self { config, options }
    }
}

impl ElectricitySource for AirconSource<'_> {
    type Target = AirconLocation;

    fn read(
        &self,
        context: &ReadContext<'_>,
    ) -> Result<SourceReading<AirconLocation>, SourceError<AirconLocation>> {
        context.check()?;
        let (client, target) =
            auth::aircon_session_controlled(self.config, context.control(), self.options)
                .map_err(|error| SourceError::from_operation(error, None))?;
        context.check()?;
        let (building, floor, room) = target.selected().map_err(|error| SourceError::Failed {
            error,
            target: None,
        })?;
        let location = AirconLocation {
            building,
            floor,
            room,
        };
        let reading = aircon::query_meter(&client, target).map_err(|error| SourceError::Failed {
            error,
            target: Some(location),
        });
        context.check()?;
        reading
    }
}
