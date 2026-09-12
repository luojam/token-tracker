use crate::support::{TempTree, prefix};
use token_tracker::adapters::codex::{CodexSessionDiscovery, CodexSessionParser};
use token_tracker::adapters::files::FileSessionSource;
use token_tracker::application::{UsageReadStore, synchronize_sessions_at};
use token_tracker::domain::Timestamp;
use token_tracker::storage::SqliteUsageStore;

#[test]
fn switching_from_turn_totals_to_responses_keeps_imported_keys() {
    let tree = TempTree::new();
    let source = include_str!("../fixtures/codex/upgrade-response-first.jsonl");
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let sync = |store: &mut SqliteUsageStore, source: &str, time| {
        tree.write("rollout-active.jsonl", source);
        synchronize_sessions_at(
            &FileSessionSource::new(
                &CodexSessionDiscovery::new([&tree.root]),
                &CodexSessionParser::new(),
            ),
            store,
            Timestamp::from_unix_milliseconds(time),
        )
        .unwrap()
    };
    sync(&mut store, &prefix(source, 8), 1);
    let legacy = store.usage_snapshot().unwrap().observations.remove(0);
    assert_eq!(
        legacy.event.identity.adapter_key,
        "legacy-turn-v1:turn-legacy-a"
    );
    let response = sync(&mut store, &prefix(source, 12), 2);
    assert_eq!(response.counts.observations_inserted, 1);
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.observations.len(), 2);
    assert!(snapshot.observations.contains(&legacy));
    assert!(
        snapshot
            .observations
            .iter()
            .any(|o| o.event.identity.adapter_key == "response-v1:response-a")
    );
    let cut = source.trim_end().len() - 1;
    assert_eq!(
        sync(&mut store, &source[..cut], 3)
            .counts
            .partial_sources_imported,
        1
    );
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
    assert_eq!(
        sync(&mut store, source, 4).counts.partial_sources_imported,
        0
    );
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
}
