use rusqlite::Connection;
use token_tracker::adapters::hermes::{HermesReadError, HermesSessionSource, read_snapshot};
use token_tracker::application::{
    SessionSource, SnapshotCompletion, SynchronizationReport, UsageReadStore, UsageStore,
    summarize_usage, synchronize_sessions_at,
};
use token_tracker::domain::{ParentSession, Timestamp, TokenCounts, UsageKind};
use token_tracker::pricing::calculate_estimate;
use token_tracker::storage::SqliteUsageStore;

use crate::support::TempTree;

fn fixture(tree: &TempTree) -> (std::path::PathBuf, Connection) {
    let path = tree.root.join("state.db");
    let connection = crate::support::hermes::database(&path);
    (path, connection)
}

#[test]
fn snapshot_normalizes_disjoint_model_task_usage_and_revises_deterministically() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    let first = read_snapshot(&path).unwrap();
    assert_eq!(first.sessions.len(), 2);
    let child = first.sessions[0].snapshot.as_ref().unwrap();
    let session = &child.session;
    assert_eq!(session.completion, SnapshotCompletion::Complete);
    assert_eq!(session.metadata.agent.as_str(), "hermes");
    assert_eq!(session.metadata.session_id, "child");
    assert_eq!(
        session.metadata.started_at.as_unix_milliseconds(),
        1700000000125
    );
    assert_eq!(
        session.metadata.working_directory.as_deref(),
        Some(std::path::Path::new("/workspace"))
    );
    assert_eq!(session.metadata.name.as_deref(), Some("Synthetic session"));
    assert_eq!(
        session.metadata.parent_session,
        Some(ParentSession::SessionId("parent".into()))
    );
    assert_eq!(session.events.len(), 5);

    let total = session
        .events
        .iter()
        .fold(TokenCounts::default(), |total, event| {
            total.checked_add(event.tokens).unwrap()
        });
    assert_eq!(
        total,
        TokenCounts {
            input: 172,
            output: 60,
            cache_read: 710,
            cache_write: 32
        }
    );

    let main_a = session
        .events
        .iter()
        .find(|event| {
            event.kind == UsageKind::Assistant
                && event
                    .attribution
                    .as_ref()
                    .is_some_and(|model| model.model == "model-a")
        })
        .unwrap();
    assert_eq!(main_a.timestamp, session.metadata.started_at);

    let main_b = session
        .events
        .iter()
        .find(|event| {
            event
                .attribution
                .as_ref()
                .is_some_and(|model| model.model == "model-b")
        })
        .unwrap();
    assert_eq!(main_b.timestamp.as_unix_milliseconds(), 1700000002500);

    let compression = session
        .events
        .iter()
        .find(|event| event.kind == UsageKind::Compaction)
        .unwrap();
    assert_eq!(compression.attribution.as_ref().unwrap().provider, "auto");

    assert!(
        session
            .events
            .iter()
            .any(|event| event.kind == UsageKind::Other)
    );
    assert_eq!(
        session
            .events
            .iter()
            .find(|event| event.attribution.is_none())
            .unwrap()
            .tokens,
        TokenCounts {
            input: 20,
            ..TokenCounts::default()
        }
    );

    assert_eq!(
        session
            .notices
            .iter()
            .map(|notice| notice.code.as_str())
            .collect::<Vec<_>>(),
        [
            "hermes_unattributed_residual",
            "hermes_counter_mismatch",
            "hermes_subscription_estimate"
        ]
    );

    assert!(
        session
            .events
            .iter()
            .all(|event| event.recorded_cost.is_none())
    );

    let empty = &first.sessions[1].snapshot.as_ref().unwrap().session;
    assert!(empty.events.is_empty());
    assert!(empty.metadata.working_directory.is_none());

    // Neither database location, physical row order nor unrelated columns define a revision.
    connection
        .execute_batch(
            "UPDATE sessions SET model_config = X'FF';
        CREATE TEMP TABLE reordered AS SELECT * FROM session_model_usage;
        DELETE FROM session_model_usage;
        INSERT INTO session_model_usage SELECT * FROM reordered ORDER BY task DESC, model DESC;",
        )
        .unwrap();
    let copy = tree.root.join("copy.db");
    std::fs::copy(&path, &copy).unwrap();

    let copied = read_snapshot(&copy).unwrap();
    assert_eq!(copied.sessions[0].key, first.sessions[0].key);
    assert_eq!(copied.sessions[0].snapshot.as_ref().unwrap(), child);

    connection.execute_batch("UPDATE session_model_usage SET input_tokens = 110 WHERE model = 'model-a' AND task = ''; ").unwrap();
    let corrected = read_snapshot(&path).unwrap();
    let corrected = corrected.sessions[0].snapshot.as_ref().unwrap();
    assert_ne!(corrected.revision, child.revision);
    let corrected_main = corrected
        .session
        .events
        .iter()
        .find(|event| event.identity == main_a.identity)
        .unwrap();
    assert_eq!(corrected_main.tokens.input, 110);

    connection
        .execute_batch("UPDATE session_model_usage SET cost_status = 'included';")
        .unwrap();

    let cost_changed = read_snapshot(&path).unwrap();
    assert_ne!(
        cost_changed.sessions[0].snapshot.as_ref().unwrap().revision,
        corrected.revision
    );
}

#[test]
fn unsupported_schema_and_invalid_accounting_cannot_become_empty_usage() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    connection
        .execute_batch("ALTER TABLE session_model_usage RENAME COLUMN task TO legacy_task;")
        .unwrap();

    let error = read_snapshot(&path).unwrap_err();
    assert!(
        matches!(&error, HermesReadError::UnsupportedSchema { table: "session_model_usage", missing } if missing == &["task"])
    );
    assert!(error.to_string().contains("per-model/task accounting"));

    connection
        .execute_batch(
            "ALTER TABLE session_model_usage RENAME COLUMN legacy_task TO task;
        UPDATE session_model_usage SET input_tokens = -1 WHERE task = 'compression';",
        )
        .unwrap();

    let rejected = read_snapshot(&path).unwrap();
    assert!(matches!(
        &rejected.sessions[0].snapshot,
        Err(HermesReadError::InvalidField("input_tokens"))
    ));
    assert!(rejected.sessions[1].snapshot.is_ok());

    connection
        .execute_batch(
            "UPDATE session_model_usage SET input_tokens = 5 WHERE task = 'compression';
        UPDATE sessions SET started_at = 1e300 WHERE id = 'child';",
        )
        .unwrap();

    assert!(matches!(
        &read_snapshot(&path).unwrap().sessions[0].snapshot,
        Err(HermesReadError::InvalidField("started_at"))
    ));

    connection
        .execute_batch(
            "UPDATE sessions SET started_at = 1700000000 WHERE id = 'child';
        UPDATE session_model_usage SET cache_read_tokens = 'invalid' WHERE task = 'compression';",
        )
        .unwrap();

    assert!(matches!(
        &read_snapshot(&path).unwrap().sessions[0].snapshot,
        Err(HermesReadError::InvalidField("cache_read_tokens"))
    ));
    assert!(read_snapshot(&tree.root.join("missing.db")).is_err());
    assert!(!tree.root.join("missing.db").exists());
}

fn sync(
    source: &HermesSessionSource,
    store: &mut SqliteUsageStore,
    time: i64,
) -> SynchronizationReport {
    synchronize_sessions_at(source, store, Timestamp::from_unix_milliseconds(time)).unwrap()
}

fn totals(store: &SqliteUsageStore) -> TokenCounts {
    summarize_usage(&store.usage_snapshot().unwrap())
        .unwrap()
        .totals
        .tokens
}

#[test]
fn cumulative_sessions_replace_buckets_across_scans_restart_and_pruning() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    let tracker = tree.root.join("tracker.db");
    let source = HermesSessionSource::new([path.clone()]);
    let mut store = SqliteUsageStore::open(&tracker).unwrap();

    assert_eq!(sync(&source, &mut store, 1).counts.sources_imported, 2);
    let mut expected = TokenCounts {
        input: 172,
        output: 60,
        cache_read: 710,
        cache_write: 32,
    };
    assert_eq!(totals(&store), expected);
    assert_eq!(sync(&source, &mut store, 2).counts.sources_unchanged, 2);
    assert_eq!(totals(&store), expected);

    connection
        .execute_batch(
            "UPDATE sessions SET input_tokens = input_tokens + 50 WHERE id = 'child';
         UPDATE session_model_usage SET input_tokens = input_tokens + 50
         WHERE model = 'model-a' AND task = '';",
        )
        .unwrap();

    assert_eq!(sync(&source, &mut store, 3).counts.observations_updated, 1);
    expected.input += 50;
    assert_eq!(totals(&store), expected);

    connection
        .execute_batch(
            "DELETE FROM session_model_usage WHERE task = '';
         INSERT INTO session_model_usage (session_id, model, billing_provider, billing_base_url,
             input_tokens, output_tokens, cache_read_tokens, cache_write_tokens)
         VALUES ('child', 'redistributed', 'proxy', 'https://example.test', 210, 55, 650, 30);",
        )
        .unwrap();

    assert_eq!(sync(&source, &mut store, 4).counts.sources_imported, 1);
    assert_eq!(totals(&store), expected);
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.observations.len(), 3);

    drop(store);
    let mut store = SqliteUsageStore::open(&tracker).unwrap();
    let restarted = HermesSessionSource::new([path]);
    assert_eq!(sync(&restarted, &mut store, 5).counts.sources_unchanged, 2);
    assert_eq!(totals(&store), expected);

    connection
        .execute_batch(
            "UPDATE sessions SET input_tokens = 200 WHERE id = 'child';
         UPDATE session_model_usage SET input_tokens = 200 WHERE task = '';",
        )
        .unwrap();
    sync(&source, &mut store, 6);
    expected.input -= 10;
    assert_eq!(totals(&store), expected);

    connection
        .execute_batch("DELETE FROM session_model_usage; DELETE FROM sessions;")
        .unwrap();

    assert_eq!(sync(&source, &mut store, 7).counts.sources_discovered, 0);
    assert!(
        store
            .source_states(&"hermes".into())
            .unwrap()
            .iter()
            .all(|state| !state.present)
    );
    assert_eq!(totals(&store), expected);
    assert_eq!(
        summarize_usage(&store.usage_snapshot().unwrap())
            .unwrap()
            .totals
            .session_count,
        2
    );
}

#[test]
fn wal_snapshots_are_cached_read_only_and_failures_preserve_imports() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
         PRAGMA wal_checkpoint(TRUNCATE);
         UPDATE session_model_usage SET input_tokens = 15 WHERE task = 'compression';",
        )
        .unwrap();

    let wal = path.with_extension("db-wal");
    let contents = || (std::fs::read(&path).unwrap(), std::fs::read(&wal).unwrap());
    let before = contents();
    assert!(!before.1.is_empty());

    let source = HermesSessionSource::new([path.clone()]);
    let discovered = source.discover(&[]).unwrap();
    let child = discovered
        .sources
        .iter()
        .find(|discovered| source.load(discovered).unwrap().session.metadata.session_id == "child")
        .unwrap();
    let cached = source.load(child).unwrap();

    assert_eq!(contents(), before);

    connection
        .execute_batch(
            "UPDATE session_model_usage SET output_tokens = 13 WHERE task = 'compression';",
        )
        .unwrap();

    assert_eq!(source.load(child).unwrap(), cached);

    let before = contents();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    sync(&source, &mut store, 1);
    assert_eq!(
        totals(&store),
        TokenCounts {
            input: 182,
            output: 70,
            cache_read: 710,
            cache_write: 32
        }
    );
    assert_eq!(contents(), before);

    let stored = store.usage_snapshot().unwrap();
    let states = store.source_states(&"hermes".into()).unwrap();

    for sql in [
        "UPDATE session_model_usage SET input_tokens = -1 WHERE task = 'compression';",
        "ALTER TABLE session_model_usage RENAME COLUMN task TO legacy_task;",
    ] {
        connection.execute_batch(sql).unwrap();
        let before = contents();

        let report = sync(&source, &mut store, 2);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.message.contains("Hermes"))
        );
        assert_eq!(store.usage_snapshot().unwrap(), stored);
        assert_eq!(store.source_states(&"hermes".into()).unwrap(), states);
        assert_eq!(contents(), before);
    }
}

#[test]
fn duplicate_sessions_defer_conflicts_and_only_covered_absence_changes_presence() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    let copy = tree.root.join("copy.db");
    std::fs::copy(&path, &copy).unwrap();
    let source = HermesSessionSource::new([path.clone(), copy.clone()]);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    assert_eq!(sync(&source, &mut store, 1).counts.sources_imported, 2);

    let stored = store.usage_snapshot().unwrap();
    connection
        .execute_batch("UPDATE sessions SET input_tokens = 999 WHERE id = 'child';")
        .unwrap();

    let conflict = sync(&source, &mut store, 2);
    assert!(
        conflict
            .warnings
            .iter()
            .any(|warning| warning.message.contains("Conflicting Hermes copies"))
    );
    assert_eq!(store.usage_snapshot().unwrap(), stored);
    assert!(
        store
            .source_states(&"hermes".into())
            .unwrap()
            .iter()
            .all(|state| state.present)
    );

    let authoritative = HermesSessionSource::new([path.clone()]);
    assert_eq!(
        sync(&authoritative, &mut store, 3).counts.sources_imported,
        1
    );
    assert_eq!(totals(&store).input, 1011);

    let known = store.source_states(&"hermes".into()).unwrap();
    connection
        .execute_batch("UPDATE sessions SET input_tokens = -1 WHERE id = 'child';")
        .unwrap();
    let rejected_copy = source.discover(&known).unwrap();
    assert_eq!(rejected_copy.sources.len(), 1);
    assert!(rejected_copy.missing_sources.is_empty());

    connection
        .execute_batch("DELETE FROM session_model_usage; DELETE FROM sessions;")
        .unwrap();
    Connection::open(&copy)
        .unwrap()
        .execute_batch("DELETE FROM session_model_usage; DELETE FROM sessions;")
        .unwrap();

    assert!(
        HermesSessionSource::new([copy.clone()])
            .discover(&known)
            .unwrap()
            .missing_sources
            .is_empty()
    );

    std::fs::remove_file(&copy).unwrap();
    let failed = source.discover(&known).unwrap();
    assert!(!failed.warnings.is_empty());
    assert!(failed.missing_sources.is_empty());
}

#[test]
fn actual_cost_requires_complete_evidence_and_subscription_usage_is_estimated() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    connection
        .execute_batch(
            "DELETE FROM session_model_usage;
         INSERT INTO session_model_usage (session_id, model, billing_provider, task, input_tokens)
         VALUES ('empty', 'gpt-6-astra', 'openai-codex', 'compression', 100);",
        )
        .unwrap();

    for (mode, calls, amount, source, expected) in [
        ("api", 1, 2.5, "provider_cost_api", Some(2.5)),
        ("api", 1, 0.0, "provider_generation_api", Some(0.0)),
        ("subscription_included", 1, 0.0, "provider_cost_api", None),
        ("api", 2, 0.0001, "provider_cost_api", None),
        ("api", 1, -1.0, "provider_cost_api", None),
        ("api", 1, 2.5, "mixed", None),
    ] {
        connection
            .execute(
                "UPDATE session_model_usage SET billing_mode = ?1, api_call_count = ?2,
                 actual_cost_usd = ?3, cost_source = ?4, cost_status = 'actual',
                 estimated_cost_usd = 999",
                rusqlite::params![mode, calls, amount, source],
            )
            .unwrap();

        let snapshot = read_snapshot(&path).unwrap();
        let event = &snapshot.sessions[1]
            .snapshot
            .as_ref()
            .unwrap()
            .session
            .events[0];
        assert_eq!(event.recorded_cost.map(|cost| cost.as_usd()), expected);
        if expected.is_none() {
            let estimate = calculate_estimate(event).unwrap().result.unwrap();
            assert_eq!(estimate.cost.as_picodollars(), 1_000_000_000);
        }
    }

    connection
        .execute_batch(
            "UPDATE session_model_usage SET input_tokens = 300000, cost_status = 'included';",
        )
        .unwrap();

    let snapshot = read_snapshot(&path).unwrap();
    let event = &snapshot.sessions[1]
        .snapshot
        .as_ref()
        .unwrap()
        .session
        .events[0];
    assert_eq!(
        calculate_estimate(event)
            .unwrap()
            .result
            .unwrap()
            .cost
            .as_picodollars(),
        6_000_000_000_000
    );
}

#[test]
fn cumulative_subscription_usage_is_estimated_with_disclosed_assumptions() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    connection
        .execute_batch(
            "DELETE FROM session_model_usage;
         UPDATE sessions SET input_tokens = 300000, output_tokens = 10000,
             cache_read_tokens = 900000, cache_write_tokens = 0;
         INSERT INTO session_model_usage (session_id, model, billing_provider,
             billing_base_url, billing_mode, api_call_count, input_tokens,
             cache_read_tokens, output_tokens)
         SELECT id, 'gpt-6-astra', 'openai-codex', 'https://chatgpt.com/backend-api/codex',
             'subscription_included', 20, 300000, 900000, 10000 FROM sessions;",
        )
        .unwrap();

    let source = HermesSessionSource::new([path]);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    for time in [1, 2] {
        let report = sync(&source, &mut store, time);
        assert_eq!(report.warnings.len(), 1);
        let snapshot = store.usage_snapshot().unwrap();
        let summary = summarize_usage(&snapshot).unwrap();
        assert_eq!(summary.totals.estimates.priced_event_count, 2);
        assert_eq!(
            summary.totals.estimates.assumed_short_context_event_count,
            2
        );
        for observation in &snapshot.observations {
            let estimate = calculate_estimate(&observation.event)
                .unwrap()
                .result
                .unwrap();
            assert_eq!(estimate.cost.as_picodollars(), 4_400_000_000_000);
        }
    }
}

#[test]
fn pricing_requires_direct_provider_attribution() {
    let tree = TempTree::new();
    let (path, connection) = fixture(&tree);
    connection
        .execute_batch(
            "DELETE FROM session_model_usage;
         INSERT INTO session_model_usage (session_id, model, task, input_tokens)
         VALUES ('empty', 'gpt-6-astra', 'compression', 100);",
        )
        .unwrap();

    for (provider, endpoint, normalized, priced) in [
        ("auto", "https://api.openai.com/v1", "openai", true),
        (
            "auto",
            "https://api.openai.com.proxy.test/v1",
            "auto",
            false,
        ),
        ("openai", "https://proxy.test/v1", "openai", false),
    ] {
        connection
            .execute(
                "UPDATE session_model_usage SET billing_provider = ?1, billing_base_url = ?2",
                [provider, endpoint],
            )
            .unwrap();

        let snapshot = read_snapshot(&path).unwrap();
        let event = &snapshot.sessions[1]
            .snapshot
            .as_ref()
            .unwrap()
            .session
            .events[0];
        assert_eq!(event.attribution.as_ref().unwrap().provider, normalized);
        assert_eq!(event.pricing_context.is_some(), priced);
    }
}
