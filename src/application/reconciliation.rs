use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use super::{SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot};
use crate::domain::{
    AgentId, ParentSession, RecordedCost, SummaryBreakdown, SummaryGroup, SummaryTotals, Timestamp,
    TokenCounts, UsageEventIdentity, UsageSummary,
};

/// Counts each event once, preferring ancestors, then session start, ID, and normalized path.
/// Missing files retain precedence; scan and import order do not affect selection.
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
    let canonical = by_event
        .values()
        .map(|observations| select_canonical_observation(observations, &sessions, &parents))
        .collect::<Vec<_>>();
    // Stable event order also makes floating-point cost accumulation deterministic.
    for canonical in &canonical {
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
    }
    let estimates =
        crate::pricing::summarize_estimates(canonical.iter().map(|observation| &observation.event));
    Ok(UsageSummary {
        totals,
        breakdown: breakdown.into_values().collect(),
        estimates,
    })
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

fn fallback_key(session: &SessionProvenance) -> (Timestamp, &str, OsString) {
    (
        session.started_at,
        &session.key.session_id,
        session
            .key
            .source_path
            .components()
            .collect::<PathBuf>()
            .into_os_string(),
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
