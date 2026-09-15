use std::error::Error;
use std::fmt;

use super::synchronization::import_adapters;
use super::usage_totals::read_usage_totals;
use super::{ImportAdapter, ImportSynchronizationError, ImportWarning, UsageReadStore, UsageStore};
use crate::domain::UsageSummary;

/// Convenience wrapper that synchronizes adapters before reading the all-time summary.
/// Use [`super::TokenTracker::refresh`] and [`super::TokenTracker::report`] for separate operations.
pub fn run_all_time_report<S>(
    adapters: &[&dyn ImportAdapter<S>],
    store: &mut S,
    warnings: Vec<ImportWarning>,
) -> Result<AllTimeReport, AllTimeReportError>
where
    S: UsageStore + UsageReadStore,
{
    let imported =
        import_adapters(adapters, store, warnings).map_err(AllTimeReportError::Synchronization)?;
    let (summary, _) =
        read_usage_totals(store).map_err(|source| AllTimeReportError::Summary(Box::new(source)))?;
    Ok(AllTimeReport {
        summary,
        warnings: imported.warnings,
    })
}

pub struct AllTimeReport {
    pub summary: UsageSummary,
    pub warnings: Vec<ImportWarning>,
}

#[derive(Debug)]
pub enum AllTimeReportError {
    Synchronization(ImportSynchronizationError),
    Summary(Box<dyn Error + Send + Sync>),
}

impl fmt::Display for AllTimeReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Synchronization(source) => {
                write!(formatter, "session synchronization failed: {source}")
            }
            Self::Summary(source) => write!(formatter, "all-time summary failed: {source}"),
        }
    }
}

impl Error for AllTimeReportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Synchronization(source) => Some(source),
            Self::Summary(source) => Some(source.as_ref()),
        }
    }
}
