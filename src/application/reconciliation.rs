use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::Path;

use super::pricing::{
    MissingCacheWritePolicy, RATE_DATE, SNAPSHOT_ID, anthropic, calculate_estimate, estimate_tier,
};
use super::{SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot};
use crate::core::{
    AgentId, EstimateBreakdown, EstimateSummary, EstimateTotal, EstimateTotals,
    EstimateUnavailableReason, ParentSession, RawServedValue, RecordedCost, ServiceTier,
    SummaryBreakdown, SummaryGroup, SummaryTotals, TierEvidence, Timestamp, TokenCounts,
    UsageEstimate, UsageEventIdentity, UsageSummary,
};

/// Count each logical event once. Prefer its earliest known ancestor observation,
/// then session start time, session ID, and source path. Missing files retain
/// their precedence; scan times and import order never affect this projection.
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
    let mut totals = SummaryTotals {
        session_count: count(session_count)?,
        unique_usage_event_count: count(by_event.len())?,
        ..SummaryTotals::default()
    };
    let mut breakdown = BTreeMap::<(AgentId, SummaryGroup), SummaryBreakdown>::new();
    let mut estimates = BTreeMap::<AgentId, EstimateSummary>::new();
    let mut estimate_rows = BTreeMap::<(AgentId, SummaryGroup, ServiceTier), EstimateTotals>::new();
    // Stable event order also makes floating-point cost accumulation deterministic.
    for observations in by_event.values() {
        let canonical = select_canonical_observation(observations, &sessions, &parents);
        let event = &canonical.event;
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
                unique_usage_event_count: 0,
            });
        row.tokens = add_tokens(row.tokens, event.tokens)?;
        add_cost(&mut row.recorded_cost, event.recorded_cost)?;
        row.unique_usage_event_count = row
            .unique_usage_event_count
            .checked_add(1)
            .ok_or(SummaryError::Overflow("event count"))?;

        let (estimate, tier, evidence, snapshot_id, rate_date) = match event.identity.agent.as_str()
        {
            "codex" => {
                let (tier, evidence) = event
                    .pricing_context
                    .as_ref()
                    .map(estimate_tier)
                    .unwrap_or((ServiceTier::Unknown, TierEvidence::Unknown));
                (
                    calculate_estimate(event, MissingCacheWritePolicy::TreatAsInput),
                    tier,
                    evidence,
                    SNAPSHOT_ID,
                    RATE_DATE,
                )
            }
            "claude" => {
                let tier = match event
                    .pricing_context
                    .as_ref()
                    .and_then(|context| context.anthropic.as_ref())
                    .map(|facts| &facts.service_tier)
                {
                    Some(RawServedValue::Value(value)) if value == "standard" => {
                        ServiceTier::Standard
                    }
                    Some(RawServedValue::Value(value)) => ServiceTier::Unsupported(value.clone()),
                    _ => ServiceTier::Unknown,
                };
                (
                    anthropic::calculate_estimate(event),
                    tier,
                    TierEvidence::ServedResponse,
                    anthropic::SNAPSHOT_ID,
                    anthropic::RATE_DATE,
                )
            }
            _ => continue,
        };
        let summary = estimates
            .entry(event.identity.agent.clone())
            .or_insert_with(|| EstimateSummary {
                snapshot_id: snapshot_id.into(),
                rate_date: rate_date.into(),
                totals: EstimateTotals::default(),
                breakdown: Vec::new(),
            });
        let row = estimate_rows
            .entry((event.identity.agent.clone(), group, tier))
            .or_default();
        add_estimate(&mut summary.totals, estimate, evidence);
        add_estimate(row, estimate, evidence);
    }
    for ((agent, group, tier), totals) in estimate_rows {
        estimates
            .get_mut(&agent)
            .expect("estimate rows have summaries")
            .breakdown
            .push(EstimateBreakdown {
                group,
                tier,
                totals,
            });
    }
    Ok(UsageSummary {
        totals,
        breakdown: breakdown.into_values().collect(),
        estimates,
    })
}

fn add_estimate(
    totals: &mut EstimateTotals,
    estimate: Result<UsageEstimate, EstimateUnavailableReason>,
    evidence: TierEvidence,
) {
    // Counts are bounded by the already-validated canonical event count.
    totals.imported_event_count += 1;
    match estimate {
        Ok(estimate) => {
            totals.priced_event_count += 1;
            totals.assumed_cache_write_event_count +=
                u64::from(estimate.assumed_cache_writes_as_input);
            match evidence {
                TierEvidence::RequestedSetting => totals.requested_setting_event_count += 1,
                TierEvidence::ServedResponse => totals.served_response_event_count += 1,
                TierEvidence::Unknown => totals.assumed_standard_event_count += 1,
            }
            totals.cost = match totals.cost {
                EstimateTotal::Unavailable => EstimateTotal::Available(estimate.cost),
                EstimateTotal::Available(current) => current
                    .checked_add(estimate.cost)
                    .map(EstimateTotal::Available)
                    .unwrap_or(EstimateTotal::Overflow),
                EstimateTotal::Overflow => EstimateTotal::Overflow,
            };
        }
        Err(reason) => *totals.unavailable_reasons.entry(reason).or_default() += 1,
    }
}

fn resolve_session_parents(
    sessions: &HashMap<SourceSessionKey, &SessionProvenance>,
) -> HashMap<SourceSessionKey, SourceSessionKey> {
    let mut by_path = HashMap::new();
    let mut by_id = HashMap::new();
    for key in sessions.keys() {
        by_path
            .entry((&key.agent, &key.source_path))
            .or_insert_with(Vec::new)
            .push(key);
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

fn fallback_key(session: &SessionProvenance) -> (Timestamp, &str, &Path) {
    (
        session.started_at,
        &session.key.session_id,
        &session.key.source_path,
    )
}

fn count(value: usize) -> Result<u64, SummaryError> {
    u64::try_from(value).map_err(|_| SummaryError::Overflow("count"))
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
    use crate::core::EstimatedCost;

    #[test]
    fn estimate_total_overflow_is_sticky_without_losing_coverage() {
        // Bundled rates cannot reach this boundary with valid summary token totals.
        let mut totals = EstimateTotals::default();
        for cost in [u128::MAX, 1, 0] {
            add_estimate(
                &mut totals,
                Ok(UsageEstimate {
                    cost: EstimatedCost::from_picodollars(cost),
                    ..UsageEstimate::default()
                }),
                TierEvidence::RequestedSetting,
            );
        }
        add_estimate(
            &mut totals,
            Err(EstimateUnavailableReason::ArithmeticOverflow),
            TierEvidence::RequestedSetting,
        );
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
