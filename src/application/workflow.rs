//! Complete import and all-time reporting workflow.

use std::error::Error;
use std::fmt;

use super::{
    ImportSynchronizationError, SessionDiscovery, SessionParser, UsageStore, UsageSummaryStore,
    render_terminal_report, synchronize_sessions,
};

/// Runs session synchronization and renders the resulting all-time summary.
pub fn run_all_time_report<D, P, S>(
    discovery: &D,
    parser: &P,
    store: &mut S,
) -> Result<String, AllTimeReportError>
where
    D: SessionDiscovery,
    P: SessionParser,
    S: UsageStore + UsageSummaryStore,
{
    let synchronization = synchronize_sessions(discovery, parser, store)
        .map_err(AllTimeReportError::Synchronization)?;
    let summary = store
        .all_time_summary()
        .map_err(|source| AllTimeReportError::Summary(Box::new(source)))?;

    Ok(render_terminal_report(&summary, &synchronization.warnings))
}

/// A failure that prevents the complete report workflow from producing a summary.
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
