//! Cancellation stays internal so existing query error and output contracts remain stable.
use crate::QueryError;
use std::time::Duration;

pub(crate) enum OperationError {
    Query(QueryError),
    Cancelled,
}

impl From<QueryError> for OperationError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl OperationError {
    pub(crate) fn into_query(self) -> QueryError {
        match self {
            Self::Query(error) => error,
            Self::Cancelled => QueryError::Response("查询已取消。"),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct OperationControl<'a> {
    pub timeout: Duration,
    cancelled: &'a dyn Fn() -> Result<bool, QueryError>,
}

impl<'a> OperationControl<'a> {
    pub(crate) fn new(
        timeout: Duration,
        cancelled: &'a dyn Fn() -> Result<bool, QueryError>,
    ) -> Self {
        Self { timeout, cancelled }
    }

    pub(crate) fn uninterrupted(timeout: Duration) -> Self {
        Self::new(timeout, &never_cancelled)
    }

    pub(crate) fn check(&self) -> Result<(), OperationError> {
        if (self.cancelled)()? {
            Err(OperationError::Cancelled)
        } else {
            Ok(())
        }
    }
}

fn never_cancelled() -> Result<bool, QueryError> {
    Ok(false)
}
