//! Default dorm source: reuse the verified protocol and existing authentication retry.
use super::{ElectricitySource, ReadContext, SourceError, SourceReading};
use crate::{Location, QueryError, dorm::with_dorm_auth_controlled, query};
use std::{collections::BTreeMap, path::Path};

pub struct DormSource<'a> {
    config: &'a Path,
    overrides: &'a BTreeMap<String, String>,
}

impl<'a> DormSource<'a> {
    pub fn new(config: &'a Path, overrides: &'a BTreeMap<String, String>) -> Self {
        Self { config, overrides }
    }
}

impl ElectricitySource for DormSource<'_> {
    type Target = Location;

    fn read(
        &self,
        context: &ReadContext<'_>,
    ) -> Result<SourceReading<Location>, SourceError<Location>> {
        context.check()?;
        let mut target = None;
        let result = with_dorm_auth_controlled(
            self.config,
            context.control(),
            self.overrides,
            |config, client| {
                if config.form.values().any(|value| value == "_") {
                    return Err(QueryError::Config(
                        "请配置完整 dorm_electricity 目标，或使用 --campus --building --floor --room 指定。",
                    ));
                }
                let location = config.location()?;
                target = Some(location.clone());
                Ok(SourceReading {
                    reading: query(config, client)?,
                    target: location,
                    token_expires_at_claim: config.expiry_claim(),
                })
            },
        );
        context.check()?;
        result.map_err(|error| SourceError::from_operation(error, target))
    }
}
