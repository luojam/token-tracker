use super::*;
use crate::application::{
    DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, ParsedSession,
};
use crate::core::{
    AgentId, ModelAttribution, RecordedCost, SessionMetadata, TokenCounts, UsageEventIdentity,
};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_DATABASE: AtomicU64 = AtomicU64::new(0);

struct TempDatabase {
    directory: PathBuf,
    path: PathBuf,
}

impl TempDatabase {
    fn new() -> Self {
        let sequence = NEXT_TEMP_DATABASE.fetch_add(1, Ordering::Relaxed);
        let directory = env::temp_dir().join(format!(
            "token-tracker-sqlite-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("usage.db");
        Self { directory, path }
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn session_import(path: &str, input_tokens: u64) -> SessionImport {
    SessionImport {
        source: DiscoveredSessionFile {
            path: PathBuf::from(path),
            revision: FileRevision {
                size: 123,
                modified_at: UNIX_EPOCH + Duration::new(1_700_000_000, 123),
            },
        },
        scanned_at: Timestamp::from_unix_milliseconds(1_700_000_001_000),
        parsed: ParsedSession {
            metadata: SessionMetadata {
                agent: AgentId::from("pi"),
                session_id: format!("session-{path}"),
                format_version: Some("3".into()),
                working_directory: Some(PathBuf::from("/work/project")),
                started_at: Timestamp::from_unix_milliseconds(1_700_000_000_000),
                name: Some("Stored session".into()),
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
            }],
            completion: ParseCompletion::Complete,
        },
    }
}

#[test]
fn default_path_prefers_xdg_and_falls_back_to_home() {
    assert_eq!(
        default_database_path_from(Some(OsStr::new("/data")), Some(OsStr::new("/home/me")))
            .unwrap(),
        PathBuf::from("/data/token-tracker/usage.db")
    );
    assert_eq!(
        default_database_path_from(None, Some(OsStr::new("/home/me"))).unwrap(),
        PathBuf::from("/home/me/.local/share/token-tracker/usage.db")
    );
    assert_eq!(
        default_database_path_from(
            Some(OsStr::new("relative-data")),
            Some(OsStr::new("/home/me")),
        )
        .unwrap(),
        PathBuf::from("/home/me/.local/share/token-tracker/usage.db")
    );
}

#[test]
fn default_path_rejects_an_invalid_home() {
    for home in [OsStr::new(""), OsStr::new("relative-home")] {
        assert!(matches!(
            default_database_path_from(None, Some(home)),
            Err(SqliteStoreError::HomeDirectoryUnavailable)
        ));
    }
}

#[test]
fn repeated_import_is_idempotent_and_source_values_can_change() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let original = session_import("/sessions/a.jsonl", 10);

    assert_eq!(
        store.commit_import(&original).unwrap(),
        CommitImportOutcome::Applied(ImportStats {
            event_identities_inserted: 1,
            observations_inserted: 1,
            observations_updated: 0,
        })
    );
    assert_eq!(
        store.commit_import(&original).unwrap(),
        CommitImportOutcome::Applied(ImportStats::default())
    );

    let mut repeated = original.clone();
    repeated.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
    assert_eq!(
        store.commit_import(&repeated).unwrap(),
        CommitImportOutcome::Applied(ImportStats::default())
    );

    let mut changed = session_import("/sessions/a.jsonl", 99);
    changed.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
    assert_eq!(
        store.commit_import(&changed).unwrap(),
        CommitImportOutcome::Applied(ImportStats {
            event_identities_inserted: 0,
            observations_inserted: 0,
            observations_updated: 1,
        })
    );
}

#[test]
fn a_newer_session_can_replace_metadata_at_the_same_source_path() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let original = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&original).unwrap();

    let mut replacement = session_import("/sessions/a.jsonl", 99);
    replacement.parsed.metadata.session_id = "replacement-session".into();
    replacement.parsed.metadata.working_directory = Some(PathBuf::from("/work/replacement"));
    replacement.parsed.metadata.started_at = Timestamp::from_unix_milliseconds(1_700_000_001_000);
    replacement.parsed.metadata.name = Some("Replacement session".into());
    replacement.parsed.metadata.parent_session =
        Some(ParentSession::SessionId("parent-session".into()));
    replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);

    assert_eq!(
        store.commit_import(&replacement).unwrap(),
        CommitImportOutcome::Applied(ImportStats {
            event_identities_inserted: 0,
            observations_inserted: 1,
            observations_updated: 0,
        })
    );
    let stored_metadata = store
        .connection
        .query_row(
            "SELECT session_id, working_directory, started_at_ms, name, parent_session
               FROM sources",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(stored_metadata.0, "replacement-session");
    assert_eq!(
        decode_path(stored_metadata.1),
        PathBuf::from("/work/replacement")
    );
    assert_eq!(stored_metadata.2, 1_700_000_001_000);
    assert_eq!(stored_metadata.3.as_deref(), Some("Replacement session"));
    assert_eq!(
        stored_metadata.4.as_deref(),
        Some(b"parent-session".as_slice())
    );
    let mut statement = store
        .connection
        .prepare("SELECT input_tokens FROM source_observations ORDER BY input_tokens")
        .unwrap();
    let stored_tokens = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|value| decode_u64(&value.unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(stored_tokens, vec![10, 99]);
}

#[test]
fn a_late_import_from_the_replaced_session_is_ignored() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let original = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&original).unwrap();

    let mut replacement = session_import("/sessions/a.jsonl", 99);
    replacement.parsed.metadata.session_id = "replacement-session".into();
    replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
    store.commit_import(&replacement).unwrap();

    let mut late_original = original;
    late_original.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
    assert_eq!(
        store.commit_import(&late_original).unwrap(),
        CommitImportOutcome::IgnoredStale
    );

    let stored_session: String = store
        .connection
        .query_row("SELECT session_id FROM sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored_session, "replacement-session");
    let mut statement = store
        .connection
        .prepare("SELECT input_tokens FROM source_observations ORDER BY input_tokens")
        .unwrap();
    let stored_tokens = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|value| decode_u64(&value.unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(stored_tokens, vec![10, 99]);
}

#[test]
fn a_stale_first_import_cannot_claim_a_newer_discovered_source() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let stale = session_import("/sessions/a.jsonl", 10);
    store
        .record_discovery(
            &AgentId::from("pi"),
            &DiscoveryReport {
                files: vec![stale.source.clone()],
                warnings: Vec::new(),
                coverage: DiscoveryCoverage {
                    inspected_roots: vec![PathBuf::from("/sessions")],
                    inaccessible_paths: Vec::new(),
                },
            },
            Timestamp::from_unix_milliseconds(1_700_000_003_000),
        )
        .unwrap();
    assert_eq!(
        store.commit_import(&stale).unwrap(),
        CommitImportOutcome::IgnoredStale
    );

    let mut current = session_import("/sessions/a.jsonl", 99);
    current.parsed.metadata.session_id = "current-session".into();
    current.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_004_000);
    assert_eq!(
        store.commit_import(&current).unwrap(),
        CommitImportOutcome::Applied(ImportStats {
            event_identities_inserted: 1,
            observations_inserted: 1,
            observations_updated: 0,
        })
    );
    let stored_session: String = store
        .connection
        .query_row("SELECT session_id FROM sources", [], |row| row.get(0))
        .unwrap();
    assert_eq!(stored_session, "current-session");
}

#[test]
fn stale_imports_and_discoveries_do_not_regress_source_state() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let original = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&original).unwrap();

    let mut newest = session_import("/sessions/a.jsonl", 99);
    newest.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
    store.commit_import(&newest).unwrap();

    let mut stale = session_import("/sessions/a.jsonl", 50);
    stale.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
    assert_eq!(
        store.commit_import(&stale).unwrap(),
        CommitImportOutcome::IgnoredStale
    );

    store
        .record_discovery(
            &AgentId::from("pi"),
            &DiscoveryReport {
                files: Vec::new(),
                warnings: Vec::new(),
                coverage: DiscoveryCoverage {
                    inspected_roots: vec![PathBuf::from("/sessions")],
                    inaccessible_paths: Vec::new(),
                },
            },
            Timestamp::from_unix_milliseconds(1_700_000_005_000),
        )
        .unwrap();
    store
        .record_discovery(
            &AgentId::from("pi"),
            &DiscoveryReport {
                files: vec![stale.source],
                warnings: Vec::new(),
                coverage: DiscoveryCoverage {
                    inspected_roots: vec![PathBuf::from("/sessions")],
                    inaccessible_paths: Vec::new(),
                },
            },
            Timestamp::from_unix_milliseconds(1_700_000_004_000),
        )
        .unwrap();

    let states = store.source_states(&AgentId::from("pi")).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(
        states[0].last_successful_scan,
        Some(Timestamp::from_unix_milliseconds(1_700_000_003_000))
    );
    assert!(!states[0].present);
    let stored_tokens = store
        .connection
        .query_row("SELECT input_tokens FROM source_observations", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .unwrap();
    assert_eq!(decode_u64(&stored_tokens).unwrap(), 99);
}

#[test]
fn reopening_a_database_preserves_imported_state() {
    let database = TempDatabase::new();
    {
        let mut store = SqliteUsageStore::open(&database.path).unwrap();
        store
            .commit_import(&session_import("/sessions/a.jsonl", u64::MAX))
            .unwrap();
    }

    let store = SqliteUsageStore::open(&database.path).unwrap();
    let states = store.source_states(&AgentId::from("pi")).unwrap();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].path, PathBuf::from("/sessions/a.jsonl"));
    assert_eq!(states[0].last_imported_revision.as_ref().unwrap().size, 123);
    assert_eq!(
        states[0].last_successful_scan,
        Some(Timestamp::from_unix_milliseconds(1_700_000_001_000))
    );

    let stored_tokens = store
        .connection
        .query_row("SELECT input_tokens FROM source_observations", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .unwrap();
    assert_eq!(decode_u64(&stored_tokens).unwrap(), u64::MAX);
}

#[test]
fn different_sources_retain_different_observations_for_one_event() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    store
        .commit_import(&session_import("/sessions/a.jsonl", 10))
        .unwrap();
    store
        .commit_import(&session_import("/sessions/b.jsonl", 99))
        .unwrap();

    let mut statement = store
        .connection
        .prepare(
            "SELECT o.input_tokens
               FROM source_observations o
               JOIN usage_events e ON e.id = o.event_id
              WHERE e.agent = 'pi' AND e.adapter_key = 'shared-event'
              ORDER BY o.input_tokens",
        )
        .unwrap();
    let values = statement
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .unwrap()
        .map(|value| decode_u64(&value.unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values, vec![10, 99]);
}

#[test]
fn conflicting_observations_are_order_independent() {
    fn stored_observations(reverse: bool) -> Vec<(PathBuf, String, u64)> {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let first = session_import("/sessions/a.jsonl", 10);
        let mut second = session_import("/sessions/b.jsonl", 99);
        second.parsed.metadata.parent_session =
            Some(ParentSession::SourcePath("/sessions/a.jsonl".into()));
        let imports = if reverse {
            [&second, &first]
        } else {
            [&first, &second]
        };
        for import in imports {
            store.commit_import(import).unwrap();
        }

        let mut statement = store
            .connection
            .prepare(
                "SELECT source.path, session.session_id, observation.input_tokens
                   FROM source_observations observation
                   JOIN sources source ON source.id = observation.source_id
                   JOIN source_sessions session
                     ON session.id = observation.source_session_id
                  ORDER BY source.path",
            )
            .unwrap();
        statement
            .query_map([], |row| {
                let path = decode_path(row.get(0)?);
                let tokens =
                    decode_u64(&row.get::<_, Vec<u8>>(2)?).map_err(to_sql_conversion_error)?;
                Ok((path, row.get(1)?, tokens))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    assert_eq!(stored_observations(false), stored_observations(true));
}

#[test]
fn a_replaced_source_keeps_the_session_provenance_of_absent_observations() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut original = session_import("/sessions/a.jsonl", 10);
    original.parsed.metadata.session_id = "original-session".into();
    store.commit_import(&original).unwrap();

    let mut replacement = session_import("/sessions/a.jsonl", 99);
    replacement.parsed.metadata.session_id = "replacement-session".into();
    replacement.parsed.events[0].identity.adapter_key = "replacement-event".into();
    replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
    store.commit_import(&replacement).unwrap();

    let mut statement = store
        .connection
        .prepare(
            "SELECT event.adapter_key, session.session_id
               FROM source_observations observation
               JOIN usage_events event ON event.id = observation.event_id
               JOIN source_sessions session
                 ON session.id = observation.source_session_id
              ORDER BY event.adapter_key",
        )
        .unwrap();
    let provenance = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(
        provenance,
        vec![
            ("replacement-event".into(), "replacement-session".into()),
            ("shared-event".into(), "original-session".into()),
        ]
    );
}

#[test]
fn a_missing_source_keeps_its_import_and_observations() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    store
        .commit_import(&session_import("/sessions/a.jsonl", 10))
        .unwrap();

    store
        .record_discovery(
            &AgentId::from("pi"),
            &DiscoveryReport {
                files: Vec::new(),
                warnings: Vec::new(),
                coverage: DiscoveryCoverage {
                    inspected_roots: vec![PathBuf::from("/sessions")],
                    inaccessible_paths: Vec::new(),
                },
            },
            Timestamp::from_unix_milliseconds(1_700_000_002_000),
        )
        .unwrap();

    let states = store.source_states(&AgentId::from("pi")).unwrap();
    assert_eq!(states.len(), 1);
    assert!(!states[0].present);
    assert!(states[0].last_imported_revision.is_some());
    let observations: i64 = store
        .connection
        .query_row("SELECT count(*) FROM source_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(observations, 1);
}

#[test]
fn pre_epoch_file_times_round_trip() {
    let time = UNIX_EPOCH - Duration::new(1, 250_000_000);
    let (seconds, nanos) = system_time_to_parts(time).unwrap();
    assert_eq!((seconds, nanos), (-2, 750_000_000));
    assert_eq!(system_time_from_parts(seconds, nanos).unwrap(), time);
}

#[test]
fn commit_failures_roll_back_and_stop_the_report_workflow() {
    use crate::adapters::pi::{PiSessionDiscovery, PiSessionParser};
    use crate::application::{
        AllTimeReportError, ImportSynchronizationError, SessionAdapter, run_all_time_report,
    };

    let database = TempDatabase::new();
    fs::write(
        database.directory.join("session.jsonl"),
        include_str!("../../../tests/fixtures/pi/all-usage.jsonl"),
    )
    .unwrap();
    let mut store = SqliteUsageStore::open(&database.path).unwrap();
    // Fail after source/session/event writes to verify the import rolls back.
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_observation BEFORE INSERT ON source_observations
         BEGIN SELECT RAISE(ABORT, 'storage write failed'); END;",
        )
        .unwrap();
    let adapter = SessionAdapter::new(
        PiSessionDiscovery::new(&database.directory),
        PiSessionParser::new(),
    );
    let result = run_all_time_report(&[&adapter], &mut store, vec![]);
    assert!(matches!(
        result,
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
            .last_imported_revision
            .is_none()
    );
}
