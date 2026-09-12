use std::error::Error;
use std::fmt;

use super::{
    ImportSynchronizationError, ImportWarning, SessionSource, SynchronizationReport,
    UsageReadStore, UsageStore, summarize_usage, synchronize_sessions,
};
use crate::domain::{AgentId, UsageSummary};

pub trait ImportAdapter<S: UsageStore> {
    fn agent_id(&self) -> AgentId;
    fn synchronize(
        &self,
        store: &mut S,
    ) -> Result<SynchronizationReport, ImportSynchronizationError>;
}

impl<A: SessionSource, S: UsageStore> ImportAdapter<S> for A {
    fn agent_id(&self) -> AgentId {
        SessionSource::agent_id(self)
    }

    fn synchronize(
        &self,
        store: &mut S,
    ) -> Result<SynchronizationReport, ImportSynchronizationError> {
        synchronize_sessions(self, store)
    }
}

pub fn run_all_time_report<S>(
    adapters: &[&dyn ImportAdapter<S>],
    store: &mut S,
    mut warnings: Vec<ImportWarning>,
) -> Result<AllTimeReport, AllTimeReportError>
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
    Ok(AllTimeReport { summary, warnings })
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
