use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use super::{SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot};
use crate::domain::{ParentSession, Timestamp, UsageEventIdentity, UsageSummary};

/// Counts each event once, preferring ancestors, then session start, ID, and normalized path.
/// Missing files retain precedence; scan and import order do not affect selection.
pub fn summarize_usage(snapshot: &UsageSnapshot) -> Result<UsageSummary, SummaryError> {
    let canonical = select_canonical_usage(snapshot)?;
    super::reporting::summarize_canonical_usage(
        canonical.session_count,
        canonical
            .observations
            .iter()
            .map(|observation| &observation.event),
    )
}

struct CanonicalUsage<'a> {
    session_count: u64,
    observations: Vec<&'a UsageObservation>,
}

fn select_canonical_usage(snapshot: &UsageSnapshot) -> Result<CanonicalUsage<'_>, SummaryError> {
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
    let observations = by_event
        .values()
        .map(|observations| select_canonical_observation(observations, &sessions, &parents))
        .collect();
    Ok(CanonicalUsage {
        session_count: count(session_count)?,
        observations,
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
