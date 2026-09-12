use std::error::Error;
use std::fmt;

use adapters::registry::ADAPTERS;
use application::{AllTimeReportError, ImportWarning, build_usage_report, run_all_time_report};
use storage::{SqliteStoreError, SqliteUsageStore};

pub mod adapters;
pub mod application;
pub mod cli;
pub mod domain;
pub mod pricing;
pub mod storage;

pub fn run() -> Result<String, TokenTrackerError> {
    let mut store = SqliteUsageStore::open_default().map_err(TokenTrackerError::StorageSetup)?;
    let mut warnings = Vec::new();
    let mut adapters = Vec::new();
    for registration in ADAPTERS {
        match (registration.factory)() {
            Ok(adapter) => adapters.push(adapter),
            Err(source) => warnings.push(ImportWarning {
                path: None,
                message: format!("{}: could not configure adapter: {source}", registration.id),
            }),
        }
    }
    let adapters = adapters.iter().map(Box::as_ref).collect::<Vec<_>>();
    let report = run_all_time_report(&adapters, &mut store, warnings)
        .map_err(TokenTrackerError::Workflow)?;
    Ok(cli::render_terminal_report(
        &build_usage_report(&report.summary),
        &report.warnings,
    ))
}

#[derive(Debug)]
pub enum TokenTrackerError {
    StorageSetup(SqliteStoreError),
    Workflow(AllTimeReportError),
}

impl fmt::Display for TokenTrackerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
            Self::StorageSetup(source) => Some(source),
            Self::Workflow(source) => Some(source),
        }
    }
}
