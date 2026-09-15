use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;

use super::{SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot};
use crate::domain::{ParentSession, Timestamp, UsageEventIdentity};

#[derive(Clone, Debug, PartialEq)]
pub struct DeduplicatedUsage<'a> {
    /// Distinct (agent, session ID) pairs, including sessions without usage.
    pub session_count: usize,
    /// Ordered by event identity for deterministic aggregation.
    pub events: Vec<DeduplicatedEvent<'a>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeduplicatedEvent<'a> {
    /// The whole selected observation, including its pricing context.
    pub canonical: &'a UsageObservation,
    /// All source/session memberships, ordered by source/session key.
    pub sessions: Vec<&'a SessionProvenance>,
}

/// Counts each event once, preferring ancestors, then session start, ID, and source key.
/// Missing sources retain precedence, regardless of scan or import order.
pub fn deduplicate_events(
    snapshot: &UsageSnapshot,
) -> Result<DeduplicatedUsage<'_>, DeduplicationError> {
    let mut sessions = HashMap::new();
    for session in &snapshot.sessions {
        if sessions.insert(session.key.clone(), session).is_some() {
            return Err(DeduplicationError::InvalidData(
                "duplicate session provenance",
            ));
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
            return Err(DeduplicationError::InvalidData(
                "invalid observation provenance",
            ));
        }
        by_event
            .entry(&observation.event.identity)
            .or_default()
            .push(observation);
    }

    let events = by_event
        .into_values()
        .map(|observations| {
            let canonical = select_canonical_observation(&observations, &sessions, &parents);
            let mut memberships = observations
                .iter()
                .map(|observation| sessions[&observation.session])
                .collect::<Vec<_>>();
            memberships.sort_by(|left, right| left.key.cmp(&right.key));
            DeduplicatedEvent {
                canonical,
                sessions: memberships,
            }
        })
        .collect();

    Ok(DeduplicatedUsage {
        session_count,
        events,
    })
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

#[derive(Debug)]
pub enum DeduplicationError {
    InvalidData(&'static str),
}

impl fmt::Display for DeduplicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(message) => write!(f, "invalid usage data: {message}"),
        }
    }
}

impl Error for DeduplicationError {}
