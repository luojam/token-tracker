//! Reusable token collection and parsing library.

use std::error::Error;
use std::fmt;

use adapters::pi::{PiDiscoveryError, PiSessionDiscovery, PiSessionParser};
use adapters::sqlite::{SqliteStoreError, SqliteUsageStore};
use application::{AllTimeReportError, run_all_time_report};

pub mod adapters;
pub mod application;
pub mod core;

/// Runs the complete MVP workflow with Pi's session root and the default database.
pub fn run() -> Result<String, TokenTrackerError> {
    let discovery =
        PiSessionDiscovery::for_default_root().map_err(TokenTrackerError::SessionDiscoverySetup)?;
    let parser = PiSessionParser::new();
    let mut store = SqliteUsageStore::open_default().map_err(TokenTrackerError::StorageSetup)?;

    run_all_time_report(&discovery, &parser, &mut store).map_err(TokenTrackerError::Workflow)
}

/// A fatal error from configuring or running the default application.
#[derive(Debug)]
pub enum TokenTrackerError {
    SessionDiscoverySetup(PiDiscoveryError),
    StorageSetup(SqliteStoreError),
    Workflow(AllTimeReportError),
}

impl fmt::Display for TokenTrackerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionDiscoverySetup(source) => {
                write!(
                    formatter,
                    "could not configure Pi session discovery: {source}"
                )
            }
            Self::StorageSetup(source) => {
                write!(formatter, "could not open usage storage: {source}")
            }
            Self::Workflow(source) => source.fmt(formatter),
        }
    }
}

impl Error for TokenTrackerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SessionDiscoverySetup(source) => Some(source),
            Self::StorageSetup(source) => Some(source),
            Self::Workflow(source) => Some(source),
        }
    }
}
