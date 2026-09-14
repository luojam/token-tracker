use std::{
    cell::RefCell,
    collections::{BTreeMap, HashSet},
    num::NonZeroU32,
    path::PathBuf,
};

use super::{HERMES_AGENT_ID, HermesReadError, discovery::DatabaseLocations, read_snapshot};
use crate::application::{
    DiscoveredSource, DiscoveryReport, DiscoveryWarning, SessionSnapshot, SessionSource, SourceKey,
    SourceState,
};
use crate::domain::AgentId;

pub struct HermesSessionSource {
    locations: DatabaseLocations,
    snapshots: RefCell<BTreeMap<SourceKey, (DiscoveredSource, SessionSnapshot)>>,
}

impl HermesSessionSource {
    /// Uses already selected database locations; discovery never scans their directories.
    pub fn new(databases: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut databases: Vec<_> = databases.into_iter().collect();
        databases.sort();
        databases.dedup();
        Self {
            locations: DatabaseLocations::Explicit(databases),
            snapshots: RefCell::default(),
        }
    }

    pub fn for_default_roots() -> std::io::Result<Self> {
        Ok(Self {
            locations: DatabaseLocations::from_environment()?,
            snapshots: RefCell::default(),
        })
    }
}

impl SessionSource for HermesSessionSource {
    type Error = HermesReadError;

    fn agent_id(&self) -> AgentId {
        HERMES_AGENT_ID.into()
    }

    fn normalization_version(&self) -> NonZeroU32 {
        NonZeroU32::new(3).unwrap()
    }

    fn discover(&self, known: &[SourceState]) -> Result<DiscoveryReport, Self::Error> {
        let mut snapshots = self.snapshots.borrow_mut();
        snapshots.clear();
        let (databases, warnings, mut complete) = self.locations.discover();
        let mut report = DiscoveryReport {
            warnings,
            ..DiscoveryReport::default()
        };
        let mut deferred = HashSet::new();

        for path in &databases {
            let database = match read_snapshot(path) {
                Ok(database) => database,
                Err(error) => {
                    complete = false;
                    report.warnings.push(DiscoveryWarning {
                        path: Some(path.clone()),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            for session in database.sessions {
                let snapshot = match session.snapshot {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        snapshots.remove(&session.key);
                        deferred.insert(session.key);
                        report.warnings.push(DiscoveryWarning {
                            path: Some(path.clone()),
                            message: error.to_string(),
                        });
                        continue;
                    }
                };
                if deferred.contains(&session.key) {
                    continue;
                }
                if let Some((_, previous)) = snapshots.get(&session.key) {
                    if previous.revision != snapshot.revision {
                        complete = false;
                        snapshots.remove(&session.key);
                        deferred.insert(session.key);
                        report.warnings.push(DiscoveryWarning {
                            path: Some(path.clone()),
                            message: "Conflicting Hermes copies of the same session; import deferred. Select one authoritative database.".into(),
                        });
                    }
                    continue;
                }
                let source = DiscoveredSource {
                    key: session.key.clone(),
                    revision: snapshot.revision.clone(),
                    path: Some(path.clone()),
                };
                snapshots.insert(session.key, (source, snapshot));
            }
        }

        report.sources = snapshots
            .values()
            .map(|(source, _)| source.clone())
            .collect();
        if complete {
            report.missing_sources = known
                .iter()
                .filter(|state| {
                    !snapshots.contains_key(&state.key)
                        && !deferred.contains(&state.key)
                        && state
                            .path
                            .as_ref()
                            .is_some_and(|path| databases.contains(path))
                })
                .map(|state| state.key.clone())
                .collect();
        }
        Ok(report)
    }

    fn load(&self, source: &DiscoveredSource) -> Result<SessionSnapshot, Self::Error> {
        self.snapshots
            .borrow()
            .get(&source.key)
            .filter(|(cached, _)| cached == source)
            .map(|(_, snapshot)| snapshot.clone())
            .ok_or(HermesReadError::UncachedSnapshot)
    }
}
