//! Read-only electricity source boundary, independent of monitoring and output formats.
mod aircon;
mod dorm;

use crate::{
    QueryError, Reading,
    control::{OperationControl, OperationError},
};
pub use aircon::{AirconLocation, AirconSource};
pub use dorm::DormSource;
use std::time::Duration;

pub struct ReadContext<'a> {
    control: OperationControl<'a>,
}

impl<'a> ReadContext<'a> {
    pub fn new(timeout: Duration) -> Self {
        Self {
            control: OperationControl::uninterrupted(timeout),
        }
    }

    pub fn with_cancellation(
        timeout: Duration,
        cancelled: &'a dyn Fn() -> Result<bool, QueryError>,
    ) -> Self {
        Self {
            control: OperationControl::new(timeout, cancelled),
        }
    }

    pub fn timeout(&self) -> Duration {
        self.control.timeout
    }

    /// Sources should check between operations; an in-flight HTTP call uses its timeout.
    pub fn is_cancelled(&self) -> Result<bool, QueryError> {
        match self.control.check() {
            Ok(()) => Ok(false),
            Err(OperationError::Cancelled) => Ok(true),
            Err(OperationError::Query(error)) => Err(error),
        }
    }

    pub(crate) fn from_control(control: &OperationControl<'a>) -> Self {
        Self { control: *control }
    }
    pub(crate) fn control(&self) -> &OperationControl<'a> {
        &self.control
    }
    pub(crate) fn check<T>(&self) -> Result<(), SourceError<T>> {
        self.control
            .check()
            .map_err(|error| SourceError::from_operation(error, None))
    }
}

#[derive(Debug)]
pub struct SourceReading<T> {
    pub reading: Reading,
    pub target: T,
    pub token_expires_at_claim: Option<String>,
}

#[derive(Debug)]
pub enum SourceError<T> {
    Cancelled,
    Failed {
        error: QueryError,
        target: Option<T>,
    },
}

impl<T> SourceError<T> {
    pub(crate) fn from_operation(error: OperationError, target: Option<T>) -> Self {
        match error {
            OperationError::Cancelled => Self::Cancelled,
            OperationError::Query(error) => Self::Failed { error, target },
        }
    }

    pub(crate) fn into_query(self) -> QueryError {
        match self {
            Self::Cancelled => QueryError::Response("查询已取消。"),
            Self::Failed { error, .. } => error,
        }
    }
}

pub trait ElectricitySource {
    type Target;
    fn read(
        &self,
        context: &ReadContext<'_>,
    ) -> Result<SourceReading<Self::Target>, SourceError<Self::Target>>;
}
