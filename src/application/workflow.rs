use std::error::Error;
use std::fmt;

use super::{
    ImportSynchronizationError, ImportWarning, SessionDiscovery, SessionParser,
    SynchronizationReport, UsageReadStore, UsageStore, render_terminal_report, summarize_usage,
    synchronize_sessions,
};
use crate::core::AgentId;

pub struct SessionAdapter<D, P> {
    discovery: D,
    parser: P,
}

impl<D: SessionDiscovery, P: SessionParser> SessionAdapter<D, P> {
    pub fn new(discovery: D, parser: P) -> Self {
        Self { discovery, parser }
    }
}

/// Allows differently typed adapters to share a workflow and storage instance.
pub trait ImportAdapter<S: UsageStore> {
    fn agent_id(&self) -> AgentId;
    fn synchronize(
        &self,
        store: &mut S,
    ) -> Result<SynchronizationReport, ImportSynchronizationError>;
}

impl<D: SessionDiscovery, P: SessionParser, S: UsageStore> ImportAdapter<S>
    for SessionAdapter<D, P>
{
    fn agent_id(&self) -> AgentId {
        self.discovery.agent_id()
    }

    fn synchronize(
        &self,
        store: &mut S,
    ) -> Result<SynchronizationReport, ImportSynchronizationError> {
        synchronize_sessions(&self.discovery, &self.parser, store)
    }
}

/// Imports all available adapters. Setup warnings come from the composition layer;
/// discovery failures become warnings, while storage failures remain fatal.
pub fn run_all_time_report<S>(
    adapters: &[&dyn ImportAdapter<S>],
    store: &mut S,
    mut warnings: Vec<ImportWarning>,
) -> Result<String, AllTimeReportError>
where
    S: UsageStore + UsageReadStore,
{
    for adapter in adapters {
        match adapter.synchronize(store) {
            Ok(report) => warnings.extend(report.warnings.into_iter().map(|mut warning| {
                warning.message = format!("{}: {}", adapter.agent_id(), warning.message);
                warning
            })),
            Err(ImportSynchronizationError::Discovery(source)) => warnings.push(ImportWarning {
                path: None,
                message: format!("{}: session discovery failed: {source}", adapter.agent_id()),
            }),
            Err(error) => return Err(AllTimeReportError::Synchronization(error)),
        }
    }
    let snapshot = store
        .usage_snapshot()
        .map_err(|source| AllTimeReportError::Summary(Box::new(source)))?;
    let summary = summarize_usage(&snapshot)
        .map_err(|source| AllTimeReportError::Summary(Box::new(source)))?;
    Ok(render_terminal_report(&summary, &warnings))
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
