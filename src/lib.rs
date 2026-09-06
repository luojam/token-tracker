use std::error::Error;
use std::fmt;

use adapters::pi::{PiSessionDiscovery, PiSessionParser};
use adapters::sqlite::{SqliteStoreError, SqliteUsageStore};
use application::{
    AllTimeReportError, ImportAdapter, ImportWarning, SessionAdapter, run_all_time_report,
};

pub mod adapters;
pub mod application;
pub mod core;

/// Runs the configured agent adapters against the default database.
pub fn run() -> Result<String, TokenTrackerError> {
    let mut store = SqliteUsageStore::open_default().map_err(TokenTrackerError::StorageSetup)?;
    let mut warnings = Vec::new();
    let mut adapters: Vec<Box<dyn ImportAdapter<SqliteUsageStore>>> = Vec::new();
    register_adapter(
        &mut adapters,
        &mut warnings,
        "pi",
        PiSessionDiscovery::for_default_root()
            .map(|discovery| SessionAdapter::new(discovery, PiSessionParser::new())),
    );
    let adapters = adapters.iter().map(Box::as_ref).collect::<Vec<_>>();
    run_all_time_report(&adapters, &mut store, warnings).map_err(TokenTrackerError::Workflow)
}

fn register_adapter<A, E>(
    adapters: &mut Vec<Box<dyn ImportAdapter<SqliteUsageStore>>>,
    warnings: &mut Vec<ImportWarning>,
    agent: &str,
    configured: Result<A, E>,
) where
    A: ImportAdapter<SqliteUsageStore> + 'static,
    E: fmt::Display,
{
    match configured {
        Ok(adapter) => adapters.push(Box::new(adapter)),
        Err(source) => warnings.push(ImportWarning {
            path: None,
            message: format!("{agent}: could not configure adapter: {source}"),
        }),
    }
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
