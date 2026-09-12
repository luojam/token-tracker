use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{self, Cursor};
use std::path::Path;

use super::adapters::{TestParser, pi_session};
use crate::support::TempTree;
use token_tracker::adapters::files::{ParseContext, SessionParser};
use token_tracker::application::{
    DiscoveredSource, DiscoveryReport, SessionSnapshot, SessionSource, SnapshotCompletion,
    SourceKey, SourceRevision, SourceState, UsageReadStore, UsageStore, summarize_usage,
    synchronize_sessions_at,
};
use token_tracker::domain::{AgentId, Timestamp};
use token_tracker::storage::SqliteUsageStore;

#[derive(Default)]
struct MemorySource {
    discovery: DiscoveryReport,
    snapshots: BTreeMap<SourceKey, SessionSnapshot>,
    loads: Cell<usize>,
}

impl SessionSource for MemorySource {
    type Error = io::Error;

    fn agent_id(&self) -> AgentId {
        "memory".into()
    }

    fn discover(&self, _known: &[SourceState]) -> Result<DiscoveryReport, Self::Error> {
        Ok(self.discovery.clone())
    }

    fn load(&self, source: &DiscoveredSource) -> Result<SessionSnapshot, Self::Error> {
        self.loads.set(self.loads.get() + 1);
        self.snapshots
            .get(&source.key)
            .cloned()
            .ok_or_else(|| io::Error::other("session unavailable"))
    }
}

impl MemorySource {
    fn insert(&mut self, id: &str, input: u64) -> SourceKey {
        let key = SourceKey(id.as_bytes().to_vec());
        let revision = SourceRevision(vec![0, 255, 128]);
        self.discovery.sources.push(DiscoveredSource {
            key: key.clone(),
            revision: revision.clone(),
            path: None,
        });
        let mut session = TestParser { agent: "memory" }
            .parse(
                &mut Cursor::new(pi_session(input)),
                ParseContext {
                    source_path: Path::new("/fixture.jsonl"),
                },
            )
            .unwrap();
        session.metadata.session_id = id.into();
        session.events[0].identity.adapter_key = id.into();
        self.snapshots
            .insert(key.clone(), SessionSnapshot { revision, session });
        key
    }
}

#[test]
fn pathless_sessions_persist_loaded_revisions_and_retry_partial_snapshots() {
    let tree = TempTree::new();
    let database = tree.root.join("usage.db");
    let mut store = SqliteUsageStore::open(&database).unwrap();
    let mut source = MemorySource::default();
    let first = source.insert("database:session-a", 10);
    let second = source.insert("database:session-b", 20);
    source.discovery.sources[0].revision = SourceRevision(b"older discovery".to_vec());
    let partial = source.snapshots.get_mut(&second).unwrap();
    partial.session.completion = SnapshotCompletion::Partial;

    let report =
        synchronize_sessions_at(&source, &mut store, Timestamp::from_unix_milliseconds(1)).unwrap();
    assert_eq!(report.counts.sources_imported, 2);
    let states = store.source_states(&source.agent_id()).unwrap();
    assert_eq!(
        states[0].last_import.as_ref().unwrap().revision,
        source.snapshots[&first].revision
    );
    let snapshot = store.usage_snapshot().unwrap();
    assert!(
        snapshot
            .sessions
            .iter()
            .all(|session| session.source_path.is_none())
    );
    assert_eq!(summarize_usage(&snapshot).unwrap().totals.tokens.input, 30);
    drop(store);

    let mut store = SqliteUsageStore::open(&database).unwrap();
    assert_eq!(store.source_states(&source.agent_id()).unwrap(), states);
    source.discovery.sources[0].revision = source.snapshots[&first].revision.clone();
    let partial = source.snapshots.get_mut(&second).unwrap();
    partial.session.events[0].tokens.input = 30;
    partial.session.completion = SnapshotCompletion::Complete;
    let report =
        synchronize_sessions_at(&source, &mut store, Timestamp::from_unix_milliseconds(2)).unwrap();
    assert_eq!(report.counts.sources_unchanged, 1);
    assert_eq!(report.counts.observations_updated, 1);
    assert_eq!(source.loads.get(), 3);
    let summary = summarize_usage(&store.usage_snapshot().unwrap()).unwrap();
    assert_eq!(summary.totals.tokens.input, 40);

    let report =
        synchronize_sessions_at(&source, &mut store, Timestamp::from_unix_milliseconds(3)).unwrap();
    assert_eq!(report.counts.sources_unchanged, 2);
    assert_eq!(source.loads.get(), 3);
}

#[test]
fn omitted_and_failed_sources_keep_history_until_absence_is_explicit() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut source = MemorySource::default();
    let key = source.insert("database:session-a", 10);
    synchronize_sessions_at(&source, &mut store, Timestamp::from_unix_milliseconds(1)).unwrap();
    let snapshot = store.usage_snapshot().unwrap();

    source.discovery.sources[0].revision = SourceRevision(b"changed".to_vec());
    source.snapshots.clear();
    let failed =
        synchronize_sessions_at(&source, &mut store, Timestamp::from_unix_milliseconds(2)).unwrap();
    assert_eq!(failed.counts.sources_failed, 1);
    let state = store.source_states(&source.agent_id()).unwrap().remove(0);
    assert!(state.present);
    assert_ne!(
        state.last_observed_revision,
        state.last_import.unwrap().revision
    );

    source.discovery.sources.clear();
    synchronize_sessions_at(&source, &mut store, Timestamp::from_unix_milliseconds(3)).unwrap();
    assert!(store.source_states(&source.agent_id()).unwrap()[0].present);

    source.discovery.missing_sources.push(key);
    synchronize_sessions_at(&source, &mut store, Timestamp::from_unix_milliseconds(4)).unwrap();
    assert!(!store.source_states(&source.agent_id()).unwrap()[0].present);
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
}
