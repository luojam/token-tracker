use crate::support::TempTree;
use rusqlite::Connection;
use std::path::PathBuf;
use token_tracker::adapters::files::{FileSessionSource, file_source_key};

use token_tracker::application::{
    CommitImportOutcome, DiscoveredSource, DiscoveryReport, ImportStats, ParseNotice, SessionData,
    SessionImport, SnapshotCompletion, SourceRevision, UsageReadStore, UsageStore,
    ValidatedSessionImport,
};
use token_tracker::domain::{
    AgentId, AnthropicBilling, CacheWriteTokens, KnownRequests, ModelAttribution, ParentSession,
    PricingContext, RecordedCost, RequestBreakdown, ServiceSpeed, ServiceTier, SessionMetadata,
    TierEvidence, Timestamp, TokenCounts, UsageEvent, UsageEventIdentity, UsageKind,
};
use token_tracker::storage::{SqliteStoreError, SqliteUsageStore};

fn session_import(path: &str, input_tokens: u64) -> SessionImport {
    SessionImport {
        normalization_version: std::num::NonZeroU32::MIN,
        source: DiscoveredSource {
            key: file_source_key(std::path::Path::new(path)),
            path: Some(PathBuf::from(path)),
            revision: SourceRevision(vec![0, 1, 255]),
        },
        scanned_at: Timestamp::from_unix_milliseconds(1_700_000_001_000),
        session: SessionData {
            metadata: SessionMetadata {
                agent: AgentId::from("pi"),
                session_id: format!("session-{path}"),

                working_directory: None,
                started_at: Timestamp::from_unix_milliseconds(1_700_000_000_000),
                name: None,
                parent_session: None,
            },
            events: vec![UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::from("pi"),
                    adapter_key: "shared-event".into(),
                },
                timestamp: Timestamp::from_unix_milliseconds(1_700_000_000_500),
                kind: UsageKind::Assistant,
                attribution: Some(ModelAttribution {
                    provider: "provider".into(),
                    model: "model".into(),
                }),
                tokens: TokenCounts {
                    input: input_tokens,
                    output: 2,
                    cache_read: 3,
                    cache_write: 4,
                },
                recorded_cost: Some(RecordedCost::from_usd(0.25).unwrap()),
                pricing_context: Some(context(TokenCounts {
                    input: input_tokens,
                    output: 2,
                    cache_read: 3,
                    cache_write: 4,
                })),
            }],
            completion: SnapshotCompletion::Complete,
            notices: vec![ParseNotice {
                code: "adapter.notice".into(),
                message: "incomplete records".into(),
                count: 2.try_into().unwrap(),
                line: Some(3.try_into().unwrap()),
            }],
        },
    }
}

fn validated(import: &SessionImport) -> ValidatedSessionImport {
    import
        .clone()
        .validate(&import.session.metadata.agent)
        .unwrap()
}

fn context(tokens: TokenCounts) -> PricingContext {
    PricingContext::Anthropic(AnthropicBilling {
        tier: ServiceTier::Standard,
        tier_evidence: TierEvidence::ServedResponse,
        speed: ServiceSpeed::Fast,
        requests: RequestBreakdown::KnownRequests(KnownRequests::new(tokens)),
        cache_writes: Some(vec![CacheWriteTokens {
            duration_seconds: 300,
            tokens: tokens.cache_write,
        }]),
    })
}

#[test]
fn imports_round_trip_and_corrections_replace_usage_and_billing() {
    let tree = TempTree::new();
    let path = tree.root.join("usage.db");
    let mut store = SqliteUsageStore::open(&path).unwrap();
    let mut import = session_import("/sessions/a.jsonl", i64::MAX as u64);
    import.source.revision = SourceRevision(vec![255, 0, 128]);
    store.commit_import(&validated(&import)).unwrap();
    let snapshot = store.usage_snapshot().unwrap();
    let states = store.source_states(&"pi".into()).unwrap();
    assert_eq!(snapshot.observations[0].event, import.session.events[0]);
    let last_import = states[0].last_import.as_ref().unwrap();
    assert_eq!(last_import.revision, import.source.revision);
    assert_eq!(last_import.notices, import.session.notices);
    drop(store);

    let mut store = SqliteUsageStore::open(&path).unwrap();
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
    assert_eq!(
        store.commit_import(&validated(&import)).unwrap(),
        CommitImportOutcome::Applied(ImportStats::default())
    );

    import.session.events[0].tokens.input = 20;
    assert!(import.clone().validate(&"pi".into()).is_err());
    import.session.events[0].pricing_context = Some(context(import.session.events[0].tokens));
    assert_eq!(
        store.commit_import(&validated(&import)).unwrap(),
        CommitImportOutcome::Applied(ImportStats {
            observations_updated: 1,
            ..ImportStats::default()
        })
    );
    assert_eq!(
        store.usage_snapshot().unwrap().observations[0].event,
        import.session.events[0]
    );
    import.session.events[0].pricing_context = None;
    import.session.notices.clear();
    store.commit_import(&validated(&import)).unwrap();
    assert_eq!(
        store.usage_snapshot().unwrap().observations[0].event,
        import.session.events[0]
    );
    assert!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .unwrap()
            .notices
            .is_empty()
    );
}

#[test]
fn replacing_a_session_preserves_provenance_and_rejects_late_imports() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let original = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&validated(&original)).unwrap();
    let mut replacement = session_import("/sessions/a.jsonl", 99);
    replacement.session.metadata.session_id = "replacement".into();
    replacement.session.metadata.parent_session = Some(ParentSession::SessionId("parent".into()));
    replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
    store.commit_import(&validated(&replacement)).unwrap();
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.sessions.len(), 2);
    let new_session = snapshot
        .sessions
        .iter()
        .find(|s| s.key.session_id == "replacement")
        .unwrap();
    assert_eq!(
        new_session.parent_session,
        replacement.session.metadata.parent_session
    );
    let mut observations: Vec<_> = snapshot
        .observations
        .iter()
        .map(|o| (o.session.session_id.as_str(), o.event.tokens.input))
        .collect();
    observations.sort();
    assert_eq!(
        observations,
        vec![
            ("replacement", 99),
            (original.session.metadata.session_id.as_str(), 10)
        ]
    );
    let states = store.source_states(&"pi".into()).unwrap();
    assert_eq!(
        store.commit_import(&validated(&original)).unwrap(),
        CommitImportOutcome::IgnoredStale
    );
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
}

#[test]
fn stale_imports_and_discoveries_cannot_regress_source_state() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let stale = session_import("/sessions/a.jsonl", 10);
    let now = Timestamp::from_unix_milliseconds(1_700_000_003_000);
    let discovered = DiscoveryReport {
        sources: vec![stale.source.clone()],
        ..DiscoveryReport::default()
    };
    store
        .record_discovery(&"pi".into(), &discovered, now)
        .unwrap();
    assert_eq!(
        store.commit_import(&validated(&stale)).unwrap(),
        CommitImportOutcome::IgnoredStale
    );
    assert!(store.usage_snapshot().unwrap().sessions.is_empty());
    let mut current = session_import("/sessions/a.jsonl", 99);
    current.scanned_at = now;
    store.commit_import(&validated(&current)).unwrap();
    let snapshot = store.usage_snapshot().unwrap();
    assert_eq!(snapshot.observations[0].event.tokens.input, 99);
    assert_eq!(
        store.commit_import(&validated(&stale)).unwrap(),
        CommitImportOutcome::IgnoredStale
    );
    store
        .record_discovery(
            &"pi".into(),
            &DiscoveryReport {
                missing_sources: vec![stale.source.key.clone()],
                ..DiscoveryReport::default()
            },
            now,
        )
        .unwrap();
    let states = store.source_states(&"pi".into()).unwrap();
    assert!(!states[0].present);
    store
        .record_discovery(&"pi".into(), &discovered, stale.scanned_at)
        .unwrap();
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
}

#[test]
fn normalization_failure_rolls_back_usage_billing_and_notices() {
    let tree = TempTree::new();
    let path = tree.root.join("usage.db");
    let mut store = SqliteUsageStore::open(&path).unwrap();
    let original = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&validated(&original)).unwrap();
    let before = store.usage_snapshot().unwrap();
    let states = store.source_states(&"pi".into()).unwrap();
    let mut replacement = session_import("/sessions/a.jsonl", 20);
    replacement.normalization_version = 2.try_into().unwrap();
    replacement.session.notices.clear();
    let mut additional = replacement.session.events[0].clone();
    additional.identity.adapter_key = "additional-event".into();
    replacement.session.events.push(additional);
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER fail_insert BEFORE INSERT ON usage_events
             WHEN NEW.adapter_key = 'additional-event'
             BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
        )
        .unwrap();
    assert!(matches!(
        store.commit_import(&validated(&replacement)),
        Err(SqliteStoreError::Sqlite(_))
    ));
    assert_eq!(store.usage_snapshot().unwrap(), before);
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
}

#[test]
fn normalization_changes_preserve_history_from_rewritten_sources() {
    for reuse_session in [false, true] {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let original = session_import("/sessions/a.jsonl", 10);
        store.commit_import(&validated(&original)).unwrap();

        let mut replacement = session_import("/sessions/a.jsonl", 20);
        if !reuse_session {
            replacement.session.metadata.session_id = "replacement-session".into();
        }
        replacement.session.events[0].identity.adapter_key = "replacement-event".into();
        replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
        store.commit_import(&validated(&replacement)).unwrap();
        let before = store.usage_snapshot().unwrap();
        assert_eq!(before.observations.len(), 2);

        replacement.normalization_version = 2.try_into().unwrap();
        replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
        store.commit_import(&validated(&replacement)).unwrap();
        assert_eq!(store.usage_snapshot().unwrap(), before);
    }
}

#[test]
fn corrupt_state_is_rejected_without_exposing_contents() {
    for sql in [
        "UPDATE import_sources SET last_successful_scan_ms = NULL",
        "UPDATE import_sources SET parse_notices = '[{\"code\":\"PRIVATE\",\"message\":\"PRIVATE\",\"count\":0}]'",
        "UPDATE billing_inputs SET facts = '{\"PRIVATE\":42}'",
        "UPDATE billing_inputs SET facts = json_set(facts, '$.anthropic.requests.known_requests[0].input', 999)",
    ] {
        let tree = TempTree::new();
        let path = tree.root.join("usage.db");
        let mut store = SqliteUsageStore::open(&path).unwrap();
        let import = session_import("/sessions/a.jsonl", 10);
        store.commit_import(&validated(&import)).unwrap();
        let connection = Connection::open(&path).unwrap();
        if sql.contains("last_successful_scan_ms") {
            assert!(connection.execute(sql, []).is_err());
            connection
                .pragma_update(None, "ignore_check_constraints", true)
                .unwrap();
        }
        connection.execute(sql, []).unwrap();
        let error = if sql.contains("billing_inputs") {
            store.usage_snapshot().unwrap_err()
        } else {
            store.source_states(&"pi".into()).unwrap_err()
        };
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
}

#[test]
fn unsupported_schema_is_left_untouched() {
    let tree = TempTree::new();
    let path = tree.root.join("usage.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE preserved(value); INSERT INTO preserved VALUES (42)")
        .unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    assert!(matches!(
        SqliteUsageStore::open(&path),
        Err(SqliteStoreError::UnsupportedSchemaVersion(99))
    ));
    assert_eq!(
        connection
            .query_row("SELECT value FROM preserved", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        42
    );
}

#[test]
fn commit_failure_rolls_back_and_stops_reporting() {
    use token_tracker::adapters::pi::{PiSessionDiscovery, PiSessionParser};
    use token_tracker::application::{
        AllTimeReportError, ImportSynchronizationError, run_all_time_report,
    };
    let tree = TempTree::new();
    tree.write(
        "session.jsonl",
        include_str!("../fixtures/pi/all-usage.jsonl"),
    );
    let path = tree.root.join("usage.db");
    let mut store = SqliteUsageStore::open(&path).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_observation BEFORE INSERT ON usage_observations
         BEGIN SELECT RAISE(ABORT, 'storage write failed'); END;",
        )
        .unwrap();
    let adapter =
        FileSessionSource::new(PiSessionDiscovery::new(&tree.root), PiSessionParser::new());
    assert!(matches!(
        run_all_time_report(&[&adapter], &mut store, vec![]),
        Err(AllTimeReportError::Synchronization(
            ImportSynchronizationError::Storage {
                operation: "committing session import",
                ..
            }
        ))
    ));
    let snapshot = store.usage_snapshot().unwrap();
    assert!(snapshot.sessions.is_empty());
    assert!(snapshot.observations.is_empty());
    assert!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .is_none()
    );
}
