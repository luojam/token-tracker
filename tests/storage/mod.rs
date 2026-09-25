use crate::support::TempTree;
use rusqlite::Connection;
use std::path::PathBuf;
use token_tracker::adapters::files::{FileSessionSource, file_source_key};

use token_tracker::application::{
    CommitImportOutcome, DiscoveredSource, DiscoveryReport, ImportStats, ObservationRetention,
    ParseNotice, ReportDiagnostic, SessionData, SessionImport, SnapshotCompletion, SourceRevision,
    UsageReadStore, UsageStore, ValidatedSessionImport,
};
use token_tracker::domain::{
    AgentId, AnthropicPricingContext, CacheWriteTokens, KnownRequests, ModelAttribution,
    ParentSession, PricingContext, RecordedCost, RequestBreakdown, ServiceSpeed, ServiceTier,
    SessionMetadata, TierEvidence, Timestamp, TokenCounts, UsageEvent, UsageEventIdentity,
    UsageKind,
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
            observation_retention: ObservationRetention::RetainOmitted,
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
    PricingContext::Anthropic(AnthropicPricingContext {
        tier: ServiceTier::Standard,
        tier_evidence: TierEvidence::ServedResponse,
        speed: ServiceSpeed::Fast,
        requests: RequestBreakdown::KnownRequests(KnownRequests::new(tokens)),
        cache_writes: Some(vec![CacheWriteTokens {
            ttl_seconds: 300,
            tokens: tokens.cache_write,
        }]),
    })
}

#[test]
fn imports_round_trip_and_corrections_replace_usage_and_pricing_context() {
    let tree = TempTree::new();
    let path = tree.root.join("usage.db");
    let mut store = SqliteUsageStore::open(&path).unwrap();
    let mut import = session_import("/sessions/a.jsonl", i64::MAX as u64);
    import.source.revision = SourceRevision(vec![255, 0, 128]);
    store.commit_import(&validated(&import)).unwrap();
    let snapshot = store.usage_snapshot().unwrap();
    let states = store.source_states(&"pi".into()).unwrap();

    assert_eq!(snapshot.observations[0].event, import.session.events[0]);
    assert_eq!(
        snapshot.diagnostics,
        vec![ReportDiagnostic {
            agent: import.session.metadata.agent.clone(),
            path: import.source.path.clone(),
            notice: import.session.notices[0].clone(),
        }]
    );
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
    assert_eq!(
        store.commit_import(&validated(&import)).unwrap(),
        CommitImportOutcome::Applied(ImportStats {
            observations_updated: 1,
            ..ImportStats::default()
        })
    );
    let corrected = store.usage_snapshot().unwrap();
    assert_eq!(corrected.observations[0].event, import.session.events[0]);
    assert!(corrected.diagnostics.is_empty());
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
fn replacing_a_session_preserves_provenance() {
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
fn normalization_failure_rolls_back_usage_pricing_context_and_notices() {
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
fn replacement_requires_complete_data_and_deletes_only_its_sessions_observations() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut import = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&validated(&import)).unwrap();

    let other = session_import("/sessions/b.jsonl", 20);
    store.commit_import(&validated(&other)).unwrap();
    let before = store.usage_snapshot().unwrap();
    let states = store.source_states(&"pi".into()).unwrap();

    import.session.observation_retention = ObservationRetention::ReplaceSessionObservations;
    import.session.completion = SnapshotCompletion::Partial;
    import.session.events.clear();

    assert_eq!(
        store.commit_import(&validated(&import)).unwrap(),
        CommitImportOutcome::DeferredIncomplete
    );
    assert_eq!(store.usage_snapshot().unwrap(), before);
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);

    import.session.completion = SnapshotCompletion::Complete;
    store.commit_import(&validated(&import)).unwrap();
    let after = store.usage_snapshot().unwrap();
    assert_eq!(after.sessions, before.sessions);
    assert_eq!(after.observations.len(), 1);
    assert_eq!(after.observations[0].event, other.session.events[0]);
}

#[test]
fn corrupt_state_is_rejected_without_exposing_contents() {
    for sql in [
        "UPDATE import_sources SET last_successful_scan_ms = NULL",
        "UPDATE import_sources SET parse_notices = '[{\"code\":\"PRIVATE\",\"message\":\"PRIVATE\",\"count\":0}]'",
        "UPDATE usage_observations SET pricing_context = '{\"PRIVATE\":42}'",
        "UPDATE usage_observations SET pricing_context = json_set(pricing_context, '$.anthropic.requests.known_requests[0].input', 999)",
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

        let error = if sql.contains("pricing_context") || sql.contains("parse_notices") {
            store.usage_snapshot().unwrap_err()
        } else {
            store.source_states(&"pi".into()).unwrap_err()
        };
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
}

#[test]
fn foreign_databases_are_rejected_without_changes() {
    for sql in [
        "CREATE TABLE preserved(value); INSERT INTO preserved VALUES (42)",
        "CREATE TABLE preserved(value); PRAGMA user_version = 1",
    ] {
        let tree = TempTree::new();
        let path = tree.root.join("foreign.db");
        Connection::open(&path).unwrap().execute_batch(sql).unwrap();
        let before = std::fs::read(&path).unwrap();

        assert!(matches!(
            SqliteUsageStore::open(&path),
            Err(SqliteStoreError::NotUsageDatabase)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn unsupported_schema_is_left_untouched() {
    let tree = TempTree::new();
    let path = tree.root.join("usage.db");
    drop(SqliteUsageStore::open(&path).unwrap());
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    let before = std::fs::read(&path).unwrap();

    assert!(matches!(
        SqliteUsageStore::open(&path),
        Err(SqliteStoreError::UnsupportedSchemaVersion(99))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn commit_failure_rolls_back_and_aborts_synchronization() {
    use token_tracker::adapters::pi::{PiSessionDiscovery, PiSessionParser};
    use token_tracker::application::{ImportSynchronizationError, synchronize_sessions};

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
        synchronize_sessions(&adapter, &mut store),
        Err(ImportSynchronizationError::Storage {
            operation: "committing session import",
            ..
        })
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
