use std::{
    error::Error,
    fmt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use super::{DeduplicationError, UsageReadStore, UsageSnapshot, deduplicate_events};
use crate::domain::export::{
    EXPORT_FORMAT_VERSION, EstimateAssumptions, ExportEstimate, ExportEvent, ExportParentSession,
    ExportSession, ExportSnapshot,
};
use crate::domain::{ParentSession, TierEvidence, UsageEvent};
use crate::storage::{MachineState, SqliteStoreError, SqliteUsageStore};

pub(crate) fn export_snapshot(
    store: &SqliteUsageStore,
    state_path: &Path,
    machine_name: Option<String>,
) -> Result<ExportSnapshot, ExportError> {
    let mut state = MachineState::open(state_path)?;
    let allocation = state.begin_export()?;
    let stored = store.usage_snapshot()?;
    let snapshot = ExportSnapshot {
        machine_id: allocation.machine_id.clone(),
        machine_name,
        export_revision: allocation.revision,
        format_version: EXPORT_FORMAT_VERSION,
        exported_at_unix_ms: i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0),
        )
        .unwrap_or(i64::MAX),
        events: build_export_events(&stored)?,
    };
    allocation.commit()?;
    Ok(snapshot)
}

fn build_export_events(snapshot: &UsageSnapshot) -> Result<Vec<ExportEvent>, ExportError> {
    let usage = deduplicate_events(snapshot).map_err(ExportError::Deduplication)?;
    Ok(usage
        .events
        .into_iter()
        .map(|selected| {
            let event = &selected.canonical.event;
            ExportEvent {
                agent: event.identity.agent.to_string(),
                event_key: event.identity.adapter_key.clone(),
                timestamp_unix_ms: event.timestamp.as_unix_milliseconds(),
                usage_kind: event.kind,
                provider: event
                    .attribution
                    .as_ref()
                    .map(|value| value.provider.clone()),
                model: event.attribution.as_ref().map(|value| value.model.clone()),
                tokens: event.tokens,
                recorded_cost_usd: event.recorded_cost.map(Into::into),
                estimate: export_estimate(event),
                pricing_context: event.pricing_context.clone(),
                sessions: selected
                    .sessions
                    .into_iter()
                    .map(|session| ExportSession {
                        session_id: session.key.session_id.clone(),
                        started_at_unix_ms: session.started_at.as_unix_milliseconds(),
                        name: session.name.clone(),
                        working_directory: session
                            .working_directory
                            .as_ref()
                            .map(|path| path.to_string_lossy().into_owned()),
                        parent_session: session.parent_session.as_ref().map(
                            |parent| match parent {
                                ParentSession::SessionId(id) => {
                                    ExportParentSession::SessionId(id.clone())
                                }
                                ParentSession::SourcePath(path) => ExportParentSession::SourcePath(
                                    path.to_string_lossy().into_owned(),
                                ),
                            },
                        ),
                    })
                    .collect(),
            }
        })
        .collect())
}

fn export_estimate(event: &UsageEvent) -> ExportEstimate {
    let Some(estimate) = crate::pricing::calculate_estimate(event) else {
        return ExportEstimate::NotNeeded;
    };
    match estimate.result {
        Ok(value) => {
            let (version, date) = estimate
                .rate_snapshot
                .expect("available estimates have a pricing snapshot");
            ExportEstimate::Available {
                cost_usd: value.cost.into(),
                pricing_version: version.into(),
                pricing_date: date.into(),
                tier: estimate.tier,
                tier_evidence: estimate.evidence,
                assumptions: EstimateAssumptions {
                    standard_rates: estimate.evidence == TierEvidence::Unknown,
                    cache_writes_as_input: value.assumed_cache_writes_as_input,
                    short_context: value.assumed_short_context,
                },
            }
        }
        Err(reason) => ExportEstimate::Unavailable {
            reason,
            pricing_version: estimate.rate_snapshot.map(|(version, _)| version.into()),
            pricing_date: estimate.rate_snapshot.map(|(_, date)| date.into()),
            tier: estimate.tier,
            tier_evidence: estimate.evidence,
        },
    }
}

#[derive(Debug)]
pub enum ExportError {
    Storage(SqliteStoreError),
    Deduplication(DeduplicationError),
}

impl fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(source) => source.fmt(formatter),
            Self::Deduplication(source) => source.fmt(formatter),
        }
    }
}

impl Error for ExportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(source) => Some(source),
            Self::Deduplication(source) => Some(source),
        }
    }
}

impl From<SqliteStoreError> for ExportError {
    fn from(source: SqliteStoreError) -> Self {
        Self::Storage(source)
    }
}
