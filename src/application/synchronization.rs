use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    CommitImportOutcome, DiscoveredSource, ImportStats, ParseNotice, ReportDiagnostic,
    SessionImport, SessionSource, SnapshotCompletion, SourceState, UsageStore,
};
use crate::domain::{AgentId, Timestamp};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportCounts {
    pub sources_discovered: u64,
    pub sources_imported: u64,
    pub sources_unchanged: u64,
    pub sources_failed: u64,
    pub partial_sources_imported: u64,
    pub event_identities_inserted: u64,
    pub observations_inserted: u64,
    pub observations_updated: u64,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportWarning {
    pub path: Option<PathBuf>,
    pub message: String,
    pub diagnostic: Option<ReportDiagnostic>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SynchronizationReport {
    pub counts: ImportCounts,
    pub warnings: Vec<ImportWarning>,
}

#[derive(Debug)]
pub enum ImportSynchronizationError {
    Discovery(Box<dyn Error + Send + Sync>),
    Storage {
        operation: &'static str,
        source: Box<dyn Error + Send + Sync>,
    },
}

impl fmt::Display for ImportSynchronizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(source) => write!(formatter, "session discovery failed: {source}"),
            Self::Storage { operation, source } => {
                write!(formatter, "storage failed while {operation}: {source}")
            }
        }
    }
}

impl Error for ImportSynchronizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Discovery(source) => Some(source.as_ref()),
            Self::Storage { source, .. } => Some(source.as_ref()),
        }
    }
}

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

pub(crate) fn synchronize_adapters<S: UsageStore>(
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
                    diagnostic: None,
                })
            }
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

pub fn synchronize_sessions<A, S>(
    source: &A,
    store: &mut S,
) -> Result<SynchronizationReport, ImportSynchronizationError>
where
    A: SessionSource,
    S: UsageStore,
{
    synchronize_sessions_at(source, store, current_timestamp())
}

pub fn synchronize_sessions_at<A, S>(
    source: &A,
    store: &mut S,
    scanned_at: Timestamp,
) -> Result<SynchronizationReport, ImportSynchronizationError>
where
    A: SessionSource,
    S: UsageStore,
{
    let agent = source.agent_id();
    let normalization_version = source.normalization_version();
    let states =
        store
            .source_states(&agent)
            .map_err(|source| ImportSynchronizationError::Storage {
                operation: "loading source state",
                source: Box::new(source),
            })?;

    let discovery_report = source
        .discover(&states)
        .map_err(|source| ImportSynchronizationError::Discovery(Box::new(source)))?;

    // Update presence even if loading fails.
    store
        .record_discovery(&agent, &discovery_report, scanned_at)
        .map_err(|source| ImportSynchronizationError::Storage {
            operation: "recording discovery",
            source: Box::new(source),
        })?;

    let known_sources = states
        .into_iter()
        .map(|state| (state.key.clone(), state))
        .collect::<HashMap<_, _>>();
    let mut sources = discovery_report.sources;
    sources.sort_by(|left, right| left.key.cmp(&right.key));

    let mut report = SynchronizationReport {
        counts: ImportCounts {
            sources_discovered: sources.len() as u64,
            ..ImportCounts::default()
        },
        warnings: discovery_report
            .warnings
            .into_iter()
            .map(|warning| ImportWarning {
                path: warning.path,
                message: warning.message,
                diagnostic: None,
            })
            .collect(),
    };

    for discovered in sources {
        if known_sources.get(&discovered.key).is_some_and(|state| {
            source_import_is_current(state, &discovered, normalization_version)
        }) {
            report.counts.sources_unchanged += 1;
            continue;
        }

        let snapshot = match source.load(&discovered) {
            Ok(result) => result,
            Err(error) => {
                report.counts.sources_failed += 1;
                report.warnings.push(ImportWarning {
                    path: discovered.path.clone(),
                    message: error.to_string(),
                    diagnostic: None,
                });
                continue;
            }
        };

        let incomplete = snapshot.session.completion == SnapshotCompletion::Partial;
        let import = SessionImport {
            normalization_version,
            source: DiscoveredSource {
                revision: snapshot.revision,
                ..discovered.clone()
            },
            scanned_at,
            session: snapshot.session,
        };

        let import = match import.validate(&agent) {
            Ok(import) => import,
            Err(error) => {
                report.counts.sources_failed += 1;
                report.warnings.push(ImportWarning {
                    path: discovered.path.clone(),
                    message: error.to_string(),
                    diagnostic: None,
                });
                continue;
            }
        };

        match store.commit_import(&import) {
            Ok(CommitImportOutcome::Applied(stats)) => {
                report.counts.sources_imported += 1;
                if incomplete {
                    report.counts.partial_sources_imported += 1;
                }
                add_import_stats(&mut report.counts, stats);
            }
            Ok(CommitImportOutcome::IgnoredStale) => {
                report.counts.sources_failed += 1;
                report.warnings.push(ImportWarning {
                    path: discovered.path.clone(),
                    message: "session import was superseded by a newer scan".into(),
                    diagnostic: None,
                });
            }
            Ok(CommitImportOutcome::DeferredIncomplete) => {
                report.counts.sources_failed += 1;
                report.warnings.push(ImportWarning {
                    path: discovered.path.clone(),
                    message: "import deferred until the session snapshot is complete".into(),
                    diagnostic: None,
                });
            }
            Err(error) => {
                return Err(ImportSynchronizationError::Storage {
                    operation: "committing session import",
                    source: Box::new(error),
                });
            }
        }
    }

    let retained_states =
        store
            .source_states(&agent)
            .map_err(|source| ImportSynchronizationError::Storage {
                operation: "loading retained parse notices",
                source: Box::new(source),
            })?;
    for state in retained_states {
        let Some(last_import) = state.last_import else {
            continue;
        };
        for notice in last_import.notices {
            report.warnings.push(ImportWarning {
                path: state.path.clone(),
                message: notice_message(&notice),
                diagnostic: Some(ReportDiagnostic {
                    agent: agent.clone(),
                    path: state.path.clone(),
                    notice,
                }),
            });
        }
    }

    report.warnings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.message.cmp(&right.message))
    });
    report.warnings.dedup();
    Ok(report)
}

fn notice_message(notice: &ParseNotice) -> String {
    let mut message = notice.message.clone();
    if let Some(line) = notice.line {
        message.push_str(&format!(" (first affected line: {line})"));
    }
    message
}

fn source_import_is_current(
    state: &SourceState,
    discovered: &DiscoveredSource,
    version: NonZeroU32,
) -> bool {
    state.last_import.as_ref().is_some_and(|import| {
        import.normalization_version == version
            && import.revision == discovered.revision
            && import.completion == SnapshotCompletion::Complete
    })
}

fn add_import_stats(counts: &mut ImportCounts, stats: ImportStats) {
    counts.event_identities_inserted = counts
        .event_identities_inserted
        .saturating_add(stats.event_identities_inserted);
    counts.observations_inserted = counts
        .observations_inserted
        .saturating_add(stats.observations_inserted);
    counts.observations_updated = counts
        .observations_updated
        .saturating_add(stats.observations_updated);
}

fn current_timestamp() -> Timestamp {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    Timestamp::from_unix_milliseconds(i64::try_from(milliseconds).unwrap_or(i64::MAX))
}
