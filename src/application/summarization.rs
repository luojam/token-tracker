use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;

use super::{
    ReportDiagnostic, SessionProvenance, SourceSessionKey, UsageObservation, UsageReadStore,
    UsageSnapshot,
};
use crate::domain::{
    AgentId, EstimateTotal, EstimateTotals, ParentSession, RecordedCost, SummaryBreakdown,
    SummaryGroup, SummaryTotals, TierEvidence, Timestamp, TokenCounts, UsageEvent,
    UsageEventIdentity, UsageSummary,
};
use crate::pricing::EventEstimate;

pub(crate) fn read_summary<S: UsageReadStore>(
    store: &S,
) -> Result<(UsageSummary, Vec<ReportDiagnostic>), ReportError> {
    let snapshot = store
        .usage_snapshot()
        .map_err(|source| ReportError::Storage(Box::new(source)))?;
    let summary = summarize_usage(&snapshot).map_err(ReportError::Summary)?;

    Ok((summary, snapshot.diagnostics))
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

/// Counts each event once, preferring ancestors, then session start, ID, and source key.
/// Missing sources retain precedence, regardless of scan or import order.
pub fn summarize_usage(snapshot: &UsageSnapshot) -> Result<UsageSummary, SummaryError> {
    let mut sessions = HashMap::new();
    for session in &snapshot.sessions {
        if sessions.insert(session.key.clone(), session).is_some() {
            return Err(SummaryError::InvalidData("duplicate session provenance"));
        }
    }

    let session_count = sessions
        .keys()
        .map(|key| (&key.agent, &key.session_id))
        .collect::<HashSet<_>>()
        .len();
    let parents = resolve_session_parents(&sessions);

    let mut by_event = BTreeMap::<&UsageEventIdentity, Vec<&UsageObservation>>::new();
    for observation in &snapshot.observations {
        if !sessions.contains_key(&observation.session)
            || observation.session.agent != observation.event.identity.agent
        {
            return Err(SummaryError::InvalidData("invalid observation provenance"));
        }
        by_event
            .entry(&observation.event.identity)
            .or_default()
            .push(observation);
    }

    // Stable event order also makes floating-point cost accumulation deterministic.
    summarize_canonical_usage(
        count(session_count)?,
        by_event.values().map(|observations| {
            &select_canonical_observation(observations, &sessions, &parents).event
        }),
    )
}

fn resolve_session_parents(
    sessions: &HashMap<SourceSessionKey, &SessionProvenance>,
) -> HashMap<SourceSessionKey, SourceSessionKey> {
    let mut by_path = HashMap::new();
    let mut by_id = HashMap::new();
    for (key, session) in sessions {
        if let Some(path) = &session.source_path {
            by_path
                .entry((&key.agent, path))
                .or_insert_with(Vec::new)
                .push(key);
        }
        by_id
            .entry((&key.agent, &key.session_id))
            .or_insert_with(Vec::new)
            .push(key);
    }

    sessions
        .values()
        .filter_map(|session| {
            let candidates = match session.parent_session.as_ref()? {
                ParentSession::SourcePath(path) => by_path.get(&(&session.key.agent, path))?,
                ParentSession::SessionId(id) => by_id.get(&(&session.key.agent, id))?,
            };
            match candidates.as_slice() {
                [parent] if **parent != session.key => {
                    Some((session.key.clone(), (*parent).clone()))
                }
                // Multiple source copies are ambiguous; use the stable fallback.
                _ => None,
            }
        })
        .collect()
}

fn select_canonical_observation<'a>(
    observations: &[&'a UsageObservation],
    sessions: &HashMap<SourceSessionKey, &SessionProvenance>,
    parents: &HashMap<SourceSessionKey, SourceSessionKey>,
) -> &'a UsageObservation {
    let mut candidates = observations
        .iter()
        .copied()
        .filter(|candidate| {
            !observations.iter().any(|other| {
                other.session != candidate.session
                    && observation_is_ancestor(other, candidate, parents)
            })
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        candidates.extend_from_slice(observations);
    }

    candidates
        .into_iter()
        .min_by_key(|observation| fallback_key(sessions[&observation.session]))
        .expect("event groups always contain an observation")
}

fn observation_is_ancestor(
    ancestor: &UsageObservation,
    descendant: &UsageObservation,
    parents: &HashMap<SourceSessionKey, SourceSessionKey>,
) -> bool {
    let mut current = &descendant.session;
    let mut visited = HashSet::new();
    let mut found = false;
    while let Some(parent) = parents.get(current) {
        if !visited.insert(current) {
            return false;
        }
        found |= *parent == ancestor.session;
        current = parent;
    }
    found
}

fn fallback_key(session: &SessionProvenance) -> (Timestamp, &str, &super::SourceKey) {
    (
        session.started_at,
        &session.key.session_id,
        &session.key.source,
    )
}

fn summarize_canonical_usage<'a>(
    session_count: u64,
    events: impl IntoIterator<Item = &'a UsageEvent>,
) -> Result<UsageSummary, SummaryError> {
    let mut totals = SummaryTotals {
        session_count,
        ..SummaryTotals::default()
    };
    let mut breakdown = BTreeMap::<(AgentId, SummaryGroup), SummaryBreakdown>::new();
    for event in events {
        totals.unique_usage_event_count = totals
            .unique_usage_event_count
            .checked_add(1)
            .ok_or(SummaryError::Overflow("event count"))?;
        totals.tokens = add_tokens(totals.tokens, event.tokens)?;
        add_cost(&mut totals.recorded_cost, event.recorded_cost)?;

        let group = match &event.attribution {
            Some(attribution) => SummaryGroup::ProviderModel(attribution.clone()),
            None => SummaryGroup::Unattributed(event.kind),
        };
        let row = breakdown
            .entry((event.identity.agent.clone(), group.clone()))
            .or_insert(SummaryBreakdown {
                agent: event.identity.agent.clone(),
                group: group.clone(),
                tokens: TokenCounts::default(),
                recorded_cost: None,
                estimates: EstimateTotals::default(),
                unique_usage_event_count: 0,
            });
        row.tokens = add_tokens(row.tokens, event.tokens)?;
        add_cost(&mut row.recorded_cost, event.recorded_cost)?;
        row.unique_usage_event_count = row
            .unique_usage_event_count
            .checked_add(1)
            .ok_or(SummaryError::Overflow("event count"))?;

        if let Some(estimate) = crate::pricing::calculate_estimate(event) {
            add_estimate(&mut totals.estimates, &estimate);
            add_estimate(&mut row.estimates, &estimate);
        }
    }

    Ok(UsageSummary {
        totals,
        breakdown: breakdown.into_values().collect(),
    })
}

fn add_estimate(totals: &mut EstimateTotals, estimate: &EventEstimate) {
    if let Some((snapshot, date)) = estimate.rate_snapshot {
        totals.rate_snapshots.insert(snapshot.into(), date.into());
    }
    totals.imported_event_count += 1;

    match estimate.result {
        Ok(value) => {
            *totals
                .tier_event_counts
                .entry(estimate.tier.clone())
                .or_default() += 1;
            totals.priced_event_count += 1;
            totals.assumed_cache_write_event_count +=
                u64::from(value.assumed_cache_writes_as_input);
            totals.assumed_short_context_event_count += u64::from(value.assumed_short_context);
            match estimate.evidence {
                TierEvidence::RequestedSetting => totals.requested_setting_event_count += 1,
                TierEvidence::ServedResponse => totals.served_response_event_count += 1,
                TierEvidence::Unknown => totals.assumed_standard_event_count += 1,
            }
            totals.cost = match totals.cost {
                EstimateTotal::Unavailable => EstimateTotal::Available(value.cost),
                EstimateTotal::Available(current) => current
                    .checked_add(value.cost)
                    .map(EstimateTotal::Available)
                    .unwrap_or(EstimateTotal::Overflow),
                EstimateTotal::Overflow => EstimateTotal::Overflow,
            };
        }
        Err(reason) => *totals.unavailable_reasons.entry(reason).or_default() += 1,
    }
}

fn add_tokens(current: TokenCounts, value: TokenCounts) -> Result<TokenCounts, SummaryError> {
    current
        .checked_add(value)
        .ok_or(SummaryError::Overflow("token total"))
}

fn add_cost(
    current: &mut Option<RecordedCost>,
    value: Option<RecordedCost>,
) -> Result<(), SummaryError> {
    if let Some(value) = value {
        *current = Some(match *current {
            Some(current) => current
                .checked_add(value)
                .map_err(|_| SummaryError::Overflow("recorded cost"))?,
            None => value,
        });
    }
    Ok(())
}

fn count(value: usize) -> Result<u64, SummaryError> {
    u64::try_from(value).map_err(|_| SummaryError::Overflow("count"))
}

#[derive(Debug)]
pub enum SummaryError {
    InvalidData(&'static str),
    Overflow(&'static str),
}

impl fmt::Display for SummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(message) => write!(f, "invalid usage data: {message}"),
            Self::Overflow(value) => write!(f, "summary {value} is out of range"),
        }
    }
}

impl Error for SummaryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EstimateUnavailableReason, EstimatedCost, ServiceTier, UsageEstimate};

    #[test]
    fn estimate_total_overflow_is_sticky_without_losing_coverage() {
        let mut totals = EstimateTotals::default();
        let mut estimate = EventEstimate {
            result: Ok(UsageEstimate::default()),
            tier: ServiceTier::Standard,
            evidence: TierEvidence::RequestedSetting,
            rate_snapshot: None,
        };
        for cost in [u128::MAX, 1, 0] {
            estimate.result = Ok(UsageEstimate {
                cost: EstimatedCost::from_picodollars(cost),
                ..UsageEstimate::default()
            });
            add_estimate(&mut totals, &estimate);
        }

        estimate.result = Err(EstimateUnavailableReason::ArithmeticOverflow);
        add_estimate(&mut totals, &estimate);

        assert_eq!(totals.cost, EstimateTotal::Overflow);
        assert_eq!(totals.imported_event_count, 4);
        assert_eq!(totals.priced_event_count, 3);
        assert_eq!(totals.requested_setting_event_count, 3);
        assert_eq!(
            totals.unavailable_reasons[&EstimateUnavailableReason::ArithmeticOverflow],
            1
        );
    }
}
