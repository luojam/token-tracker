use crate::support::{TempTree, jsonl, prefix, records};
use serde_json::json;
use std::fs;
use token_tracker::adapters::codex::{CodexSessionDiscovery, CodexSessionParser};
use token_tracker::adapters::files::FileSessionSource;
use token_tracker::application::{
    CostAmount, CostTotal, UsageReadStore, UsageStore, synchronize_sessions_at,
};
use token_tracker::domain::{EstimatedCost, Timestamp, TokenCounts};
use token_tracker::storage::SqliteUsageStore;
use token_tracker::{LocalSourceConfig, TokenTracker, TokenTrackerConfig};

#[test]
fn compression_and_resuming_preserve_one_source_and_count_new_usage_once() {
    let tree = TempTree::new();
    let rollout = include_str!("../fixtures/codex/response-mirrors.jsonl");
    let initial = format!("{}\n", prefix(rollout, 6));
    let plain = tree.write("rollout-session.jsonl", &initial);
    let compressed = plain.with_extension("jsonl.zst");
    let discovery = CodexSessionDiscovery::new([&tree.root]);
    let parser = CodexSessionParser::new();
    let source = FileSessionSource::new(&discovery, &parser);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let sync = |store: &mut SqliteUsageStore, time| {
        let report =
            synchronize_sessions_at(&source, store, Timestamp::from_unix_milliseconds(time))
                .unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        report.counts
    };
    assert_eq!(sync(&mut store, 1).observations_inserted, 1);
    let original_key = store.source_states(&"codex".into()).unwrap()[0].key.clone();

    fs::write(
        &compressed,
        zstd::stream::encode_all(initial.as_bytes(), 0).unwrap(),
    )
    .unwrap();
    fs::remove_file(&plain).unwrap();
    assert_eq!(sync(&mut store, 2).observations_inserted, 0);

    fs::write(&plain, rollout).unwrap();
    assert_eq!(sync(&mut store, 3).observations_inserted, 1);
    fs::write(
        &compressed,
        zstd::stream::encode_all(rollout.as_bytes(), 0).unwrap(),
    )
    .unwrap();
    fs::remove_file(&plain).unwrap();
    assert_eq!(sync(&mut store, 4).observations_inserted, 0);
    assert_eq!(sync(&mut store, 5).sources_unchanged, 1);

    let states = store.source_states(&"codex".into()).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].key, original_key);
    assert!(states[0].present);
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.observations.len(), 2);
    assert_eq!(
        snapshot
            .observations
            .iter()
            .map(|o| o.event.tokens.total())
            .sum::<u128>(),
        250
    );
}

#[test]
fn referenced_fork_imports_local_responses_without_parent_files() {
    let tree = TempTree::new();
    tree.write(
        "rollout-child.jsonl",
        include_str!("../fixtures/codex/referenced-fork.jsonl"),
    );
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    synchronize_sessions_at(
        &FileSessionSource::new(
            &CodexSessionDiscovery::new([&tree.root]),
            &CodexSessionParser::new(),
        ),
        &mut store,
        Timestamp::from_unix_milliseconds(1),
    )
    .unwrap();
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.observations.len(), 2);
    assert_eq!(
        snapshot
            .observations
            .iter()
            .map(|o| o.event.tokens.total())
            .sum::<u128>(),
        250
    );
}

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

#[test]
fn request_pricing_corrections_survive_reopening_with_unchanged_token_totals() {
    let tree = TempTree::new();
    let mut records = records(include_str!("../fixtures/codex/legacy-fresh.jsonl"));
    records[2]["payload"]["model"] = json!("gpt-5.5");
    for record in &mut records {
        if !record["payload"]["info"].is_object() {
            continue;
        }
        for vector in ["total_token_usage", "last_token_usage"] {
            if let Some(counters) = record["payload"]["info"][vector].as_object_mut() {
                for counter in counters.values_mut() {
                    *counter = json!(counter.as_u64().unwrap() * 2_000);
                }
            }
        }
    }
    tree.write("rollout-legacy.jsonl", jsonl(&records));
    let config = TokenTrackerConfig {
        database_path: Some(tree.root.join("usage.db")),
        sources: vec![LocalSourceConfig::Codex {
            roots: Some(vec![tree.root.clone()]),
        }],
        ..Default::default()
    };
    let mut tracker = TokenTracker::open(config.clone()).unwrap();
    tracker.refresh().unwrap();
    let original = tracker.report().unwrap();
    assert_eq!(
        original.report.totals.cost,
        CostTotal::Available {
            amount: CostAmount::Estimated(EstimatedCost::from_picodollars(3_100_000_000_000)),
            partial: false,
        }
    );
    assert_eq!(
        original.report.totals.tokens,
        TokenCounts {
            input: 240_000,
            output: 60_000,
            cache_read: 200_000,
            cache_write: 0,
        }
    );
    drop(tracker);

    let mut tracker = TokenTracker::open(config.clone()).unwrap();
    assert_eq!(tracker.report().unwrap(), original);
    records[6]["payload"]["info"]["last_token_usage"] =
        records[6]["payload"]["info"]["total_token_usage"].clone();
    records.drain(4..6);
    tree.write("rollout-legacy.jsonl", jsonl(&records));
    tracker.refresh().unwrap();

    let corrected = tracker.report().unwrap();
    assert_eq!(
        corrected.report.totals.tokens,
        original.report.totals.tokens
    );
    assert_eq!(
        corrected.report.totals.cost,
        CostTotal::Available {
            amount: CostAmount::Estimated(EstimatedCost::from_picodollars(5_300_000_000_000)),
            partial: false,
        }
    );
    drop(tracker);
    assert_eq!(
        TokenTracker::open(config).unwrap().report().unwrap(),
        corrected
    );
}
