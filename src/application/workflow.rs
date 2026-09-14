use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

use super::{
    ImportCounts, ImportSynchronizationError, ImportWarning, ParseNotice, SessionSource,
    SummaryError, SynchronizationReport, UsageReadStore, UsageStore, summarize_usage,
    synchronize_sessions,
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
    warnings: Vec<ImportWarning>,
) -> Result<AllTimeReport, AllTimeReportError>
where
    S: UsageStore + UsageReadStore,
{
    let imported =
        import_adapters(adapters, store, warnings).map_err(AllTimeReportError::Synchronization)?;
    let (summary, _) =
        read_summary(store).map_err(|source| AllTimeReportError::Summary(Box::new(source)))?;
    Ok(AllTimeReport {
        summary,
        warnings: imported.warnings,
    })
}

pub(crate) fn import_adapters<S: UsageStore>(
    adapters: &[&dyn ImportAdapter<S>],
    store: &mut S,
    warnings: Vec<ImportWarning>,
) -> Result<SynchronizationReport, ImportSynchronizationError> {
    let mut result = SynchronizationReport {
        warnings,
        ..Default::default()
    };
    for adapter in adapters {
        match adapter.synchronize(store) {
            Ok(report) => {
                result.counts.accumulate(&report.counts);
                result
                    .warnings
                    .extend(report.warnings.into_iter().map(|mut warning| {
                        warning.message = format!("{}: {}", adapter.agent_id(), warning.message);
                        warning
                    }));
            }
            Err(ImportSynchronizationError::Discovery(source)) => {
                result.warnings.push(ImportWarning {
                    path: None,
                    message: format!("{}: session discovery failed: {source}", adapter.agent_id()),
                })
            }
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

impl ImportCounts {
    fn accumulate(&mut self, other: &Self) {
        self.sources_discovered = self
            .sources_discovered
            .saturating_add(other.sources_discovered);
        self.sources_imported = self.sources_imported.saturating_add(other.sources_imported);
        self.sources_unchanged = self
            .sources_unchanged
            .saturating_add(other.sources_unchanged);
        self.sources_failed = self.sources_failed.saturating_add(other.sources_failed);
        self.partial_sources_imported = self
            .partial_sources_imported
            .saturating_add(other.partial_sources_imported);
        self.event_identities_inserted = self
            .event_identities_inserted
            .saturating_add(other.event_identities_inserted);
        self.observations_inserted = self
            .observations_inserted
            .saturating_add(other.observations_inserted);
        self.observations_updated = self
            .observations_updated
            .saturating_add(other.observations_updated);
    }
}

pub(crate) fn read_summary<S: UsageReadStore + UsageStore>(
    store: &S,
) -> Result<(UsageSummary, Vec<ReportDiagnostic>), ReportError> {
    let snapshot = store
        .usage_snapshot()
        .map_err(|source| ReportError::Storage(Box::new(source)))?;
    let summary = summarize_usage(&snapshot).map_err(ReportError::Summary)?;
    let agents = snapshot
        .sessions
        .iter()
        .map(|session| &session.key.agent)
        .collect::<BTreeSet<_>>();
    let mut diagnostics = Vec::new();
    for agent in agents {
        let states = store
            .source_states(agent)
            .map_err(|source| ReportError::Storage(Box::new(source)))?;
        for state in states {
            if let Some(import) = state.last_import {
                diagnostics.extend(import.notices.into_iter().map(|notice| ReportDiagnostic {
                    agent: agent.clone(),
                    path: state.path.clone(),
                    notice,
                }));
            }
        }
    }
    Ok((summary, diagnostics))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportDiagnostic {
    pub agent: AgentId,
    pub path: Option<PathBuf>,
    pub notice: ParseNotice,
}

#[derive(Debug)]
pub enum ReportError {
    Storage(Box<dyn Error + Send + Sync>),
    Summary(SummaryError),
}

impl fmt::Display for ReportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(source) => source.fmt(formatter),
            Self::Summary(source) => source.fmt(formatter),
        }
    }
}

impl Error for ReportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(source) => Some(source.as_ref()),
            Self::Summary(source) => Some(source),
        }
    }
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
