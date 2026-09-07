use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::Path;

use super::pricing::{RATE_DATE, SNAPSHOT_ID, calculate_estimate};
use super::{SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot};
use crate::core::{
    EstimateBreakdown, EstimateSummary, EstimateTotal, EstimateTotals, EstimateUnavailableReason,
    EstimatedCost, ModelAttribution, ParentSession, RecordedCost, ServiceTier, SummaryBreakdown,
    SummaryGroup, SummaryTotals, TierEvidence, Timestamp, TokenCounts, UsageEventIdentity,
    UsageSummary,
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
    let mut breakdown = BTreeMap::<SummaryGroup, SummaryBreakdown>::new();
    let mut estimate_totals = EstimateTotals::default();
    let mut estimate_rows =
        BTreeMap::<(Option<ModelAttribution>, ServiceTier), EstimateTotals>::new();
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
        let row = breakdown.entry(group.clone()).or_insert(SummaryBreakdown {
            group,
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

        if event.identity.agent.as_str() == "codex" {
            let estimate = calculate_estimate(event);
            let (tier, evidence) = event
                .pricing_context
                .as_ref()
                .map(|context| (context.tier.clone(), context.tier_evidence))
                .unwrap_or((ServiceTier::Unknown, TierEvidence::Unknown));
            let row = estimate_rows
                .entry((event.attribution.clone(), tier))
                .or_default();
            add_estimate(&mut estimate_totals, estimate, evidence);
            add_estimate(row, estimate, evidence);
        }
    }
    let estimate = (estimate_totals.imported_event_count > 0).then(|| EstimateSummary {
        snapshot_id: SNAPSHOT_ID.into(),
        rate_date: RATE_DATE.into(),
        totals: estimate_totals,
        breakdown: estimate_rows
            .into_iter()
            .map(|((attribution, tier), totals)| EstimateBreakdown {
                attribution,
                tier,
                totals,
            })
            .collect(),
    });
    Ok(UsageSummary {
        totals,
        breakdown: breakdown.into_values().collect(),
        estimate,
    })
}

fn add_estimate(
    totals: &mut EstimateTotals,
    estimate: Result<EstimatedCost, EstimateUnavailableReason>,
    evidence: TierEvidence,
) {
    // Counts are bounded by the already-validated canonical event count.
    totals.imported_event_count += 1;
    match estimate {
        Ok(cost) => {
            totals.priced_event_count += 1;
            match evidence {
                TierEvidence::RequestedSetting => totals.requested_setting_event_count += 1,
                TierEvidence::ServedResponse => totals.served_response_event_count += 1,
                TierEvidence::Unknown => {}
            }
            totals.cost = match totals.cost {
                EstimateTotal::Unavailable => EstimateTotal::Available(cost),
                EstimateTotal::Available(current) => current
                    .checked_add(cost)
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

    #[test]
    fn estimate_total_overflow_is_sticky_without_losing_coverage() {
        // Bundled rates cannot reach this boundary with valid summary token totals.
        let mut totals = EstimateTotals::default();
        for cost in [u128::MAX, 1, 0] {
            add_estimate(
                &mut totals,
                Ok(EstimatedCost::from_picodollars(cost)),
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
