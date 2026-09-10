use super::*;
use crate::application::{ParseNotice, ParseNoticeCode};

fn notice() -> ParseNotice {
    ParseNotice {
        code: ParseNoticeCode::IncompleteResponseUsage,
        count: 2.try_into().unwrap(),
        line: Some(3.try_into().unwrap()),
    }
}

#[test]
fn old_schema_migrations_preserve_existing_observations() {
    for version in [1, 2] {
        let database = TempDatabase::new();
        let mut connection = Connection::open(&database.path).unwrap();
        connection
            .execute_batch(include_str!("../schema_v1.sql"))
            .unwrap();
        if version == 2 {
            connection
                .execute_batch(include_str!("../schema_v2.sql"))
                .unwrap();
        }
        let import = session_import("/sessions/existing.jsonl", u64::MAX);
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "INSERT INTO sources (
            id, path, agent, last_observed_size, last_observed_modified_seconds,
            last_observed_modified_nanos, last_imported_size, last_imported_modified_seconds,
            last_imported_modified_nanos, last_discovery_scan_ms, last_successful_scan_ms,
            last_parse_completion, present
         ) VALUES (1, ?1, 'pi', ?2, 1700000000, 123, ?2, 1700000000, 123,
                   1700000001000, 1700000001000, 'complete', 1)",
                params![
                    encode_path(&import.source.path),
                    encode_u64(import.source.revision.size)
                ],
            )
            .unwrap();
        let session_id = upsert_source_session(&transaction, 1, &import).unwrap();
        transaction
        .execute(
            "INSERT INTO usage_events (id, agent, adapter_key) VALUES (1, 'pi', 'shared-event')",
            [],
        )
        .unwrap();
        let event = &import.parsed.events[0];
        transaction
            .execute(
                "INSERT INTO source_observations (
                source_id, source_session_id, event_id, timestamp_ms, usage_kind,
                provider, model, input_tokens, output_tokens, cache_read_tokens,
                cache_write_tokens, recorded_cost_usd
             ) VALUES (1, ?1, 1, ?2, 'assistant', 'provider', 'model', ?3, ?4, ?5, ?6, 0.25)",
                params![
                    session_id,
                    event.timestamp.as_unix_milliseconds(),
                    encode_u64(event.tokens.input),
                    encode_u64(event.tokens.output),
                    encode_u64(event.tokens.cache_read),
                    encode_u64(event.tokens.cache_write),
                ],
            )
            .unwrap();
        transaction.commit().unwrap();
        drop(connection);

        let store = SqliteUsageStore::open(&database.path).unwrap();
        let snapshot = store.usage_snapshot().unwrap();
        assert_eq!(snapshot.observations.len(), 1);
        assert_eq!(&snapshot.observations[0].event, event);
        assert_eq!(snapshot.observations[0].session, snapshot.sessions[0].key);
        let states = store.source_states(&"pi".into()).unwrap();
        assert!(states[0].notices.is_empty());
        assert_eq!(
            states[0].last_imported_revision,
            Some(import.source.revision)
        );
        assert_eq!(states[0].last_successful_scan, Some(import.scanned_at));
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            migrations::SCHEMA_VERSION
        );
    }
}

#[test]
fn failed_and_stale_commits_preserve_notices_with_observations() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut original = session_import("/sessions/a.jsonl", 10);
    original.parsed.notices = vec![notice()];
    store.commit_import(&original).unwrap();
    let before = store.usage_snapshot().unwrap();
    let states = store.source_states(&"pi".into()).unwrap();
    store
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_update BEFORE UPDATE ON source_observations
         BEGIN SELECT RAISE(ABORT, 'storage write failed'); END;",
        )
        .unwrap();
    let mut replacement = original.clone();
    replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
    replacement.parsed.notices.clear();
    replacement.parsed.events[0].tokens.input = 99;
    assert!(store.commit_import(&replacement).is_err());
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
    assert_eq!(store.usage_snapshot().unwrap(), before);

    replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_000_000);
    assert_eq!(
        store.commit_import(&replacement).unwrap(),
        CommitImportOutcome::IgnoredStale
    );
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
}

#[test]
fn invalid_notice_data_is_rejected_without_exposing_contents() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let mut import = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&import).unwrap();
    import.parsed.notices = vec![notice(), notice()];
    assert!(matches!(
        store.commit_import(&import),
        Err(SqliteStoreError::InvalidImport(_))
    ));
    assert!(
        store.source_states(&"pi".into()).unwrap()[0]
            .notices
            .is_empty()
    );

    for invalid in [
        r#"[{"code":"incomplete_response_usage","count":0}]"#,
        r#"[{"code":"incomplete_response_usage","count":1,"line":0}]"#,
        r#"[{"code":"PRIVATE_TEXT","count":1}]"#,
        r#"[{"code":"truncated_tail","count":1,"text":"PRIVATE_TEXT"}]"#,
        r#"[{"code":"truncated_tail","count":1},{"code":"truncated_tail","count":1}]"#,
    ] {
        store
            .connection
            .execute("UPDATE sources SET parse_notices = ?1", [invalid])
            .unwrap();
        let error = store.source_states(&"pi".into()).unwrap_err().to_string();
        assert!(!error.contains("PRIVATE_TEXT"));
    }
}
