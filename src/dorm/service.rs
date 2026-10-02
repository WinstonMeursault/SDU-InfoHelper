//! Authenticated read-only operations and single authentication retry.
use super::{Config, SelectionLevel, client, selection_form};
use crate::{
    QueryError, auth,
    control::{OperationControl, OperationError},
    settings,
};
use reqwest::blocking::Client;
use std::{collections::BTreeMap, path::Path, time::Duration};

/// Authenticated, read-only dorm operation. Renew on HTTP/business 401 once.
/// Legacy request.json remains readable, while new deployments use config.yaml.
pub fn with_dorm_auth<T>(
    path: &Path,
    timeout: Duration,
    operation: impl FnMut(&Config, &Client) -> Result<T, QueryError>,
) -> Result<T, QueryError> {
    with_dorm_auth_overrides(path, timeout, &BTreeMap::new(), operation)
}

/// Merge a temporary target before resolving directory names or querying the server.
pub fn with_dorm_auth_overrides<T>(
    path: &Path,
    timeout: Duration,
    overrides: &BTreeMap<String, String>,
    mut operation: impl FnMut(&Config, &Client) -> Result<T, QueryError>,
) -> Result<T, QueryError> {
    with_dorm_auth_depth(
        path,
        &OperationControl::uninterrupted(timeout),
        4,
        overrides,
        &mut operation,
    )
    .map_err(OperationError::into_query)
}

/// Directory lookups resolve only the ancestors required by this level.
pub fn with_dorm_directory_auth<T>(
    path: &Path,
    timeout: Duration,
    level: SelectionLevel,
    operation: impl FnMut(&Config, &Client) -> Result<T, QueryError>,
) -> Result<T, QueryError> {
    with_dorm_directory_auth_overrides(path, timeout, level, &BTreeMap::new(), operation)
}

/// Merge directory ancestors before resolving them; unused descendants are ignored.
pub fn with_dorm_directory_auth_overrides<T>(
    path: &Path,
    timeout: Duration,
    level: SelectionLevel,
    overrides: &BTreeMap<String, String>,
    mut operation: impl FnMut(&Config, &Client) -> Result<T, QueryError>,
) -> Result<T, QueryError> {
    with_dorm_auth_depth(
        path,
        &OperationControl::uninterrupted(timeout),
        level.number(),
        overrides,
        &mut operation,
    )
    .map_err(OperationError::into_query)
}

pub(crate) fn with_dorm_auth_controlled<T>(
    path: &Path,
    control: &OperationControl<'_>,
    overrides: &BTreeMap<String, String>,
    mut operation: impl FnMut(&Config, &Client) -> Result<T, QueryError>,
) -> Result<T, OperationError> {
    with_dorm_auth_depth(path, control, 4, overrides, &mut operation)
}

fn with_dorm_auth_depth<T>(
    path: &Path,
    control: &OperationControl<'_>,
    depth: usize,
    overrides: &BTreeMap<String, String>,
    operation: &mut impl FnMut(&Config, &Client) -> Result<T, QueryError>,
) -> Result<T, OperationError> {
    control.check()?;
    let client = client(control.timeout)?;
    if path.extension().is_some_and(|x| x == "json") {
        let mut config = Config::load(path)?;
        if depth == 4 {
            config = config.with_location_overrides(overrides)?;
        } else {
            selection_form(&config, selection_level(depth), overrides)?;
            config.form.extend(overrides.clone());
        }
        control.check()?;
        return operation(&config, &client).map_err(OperationError::from);
    }
    let settings = settings::Settings::read(path)?.with_dorm_overrides(overrides, depth)?;
    let token = auth::token_controlled(&settings, path, control, None)?;
    let attempt = super::directory::resolve_controlled(
        &settings.dorm_electricity,
        &token.access_token,
        &client,
        depth,
        control,
    )
    .and_then(|request| {
        control.check()?;
        operation(&request, &client).map_err(OperationError::from)
    });
    if !matches!(
        attempt,
        Err(OperationError::Query(QueryError::Authentication))
    ) {
        return attempt;
    }
    let token = auth::token_controlled(&settings, path, control, Some(&token.access_token))?;
    let request = super::directory::resolve_controlled(
        &settings.dorm_electricity,
        &token.access_token,
        &client,
        depth,
        control,
    )?;
    control.check()?;
    operation(&request, &client).map_err(OperationError::from)
}

fn selection_level(depth: usize) -> SelectionLevel {
    match depth {
        0 => SelectionLevel::Campuses,
        1 => SelectionLevel::Buildings,
        2 => SelectionLevel::Floors,
        3 => SelectionLevel::Rooms,
        _ => unreachable!("directory depth"),
    }
}
