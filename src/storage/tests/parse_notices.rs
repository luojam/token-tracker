use super::*;
use crate::application::ParseNotice;

fn notice() -> ParseNotice {
    ParseNotice {
        code: "adapter.notice".into(),
        message: "incomplete records".into(),
        count: 2.try_into().unwrap(),
        line: Some(3.try_into().unwrap()),
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
            "CREATE TRIGGER reject_update BEFORE UPDATE ON usage_observations
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
            .execute("UPDATE import_sources SET parse_notices = ?1", [invalid])
            .unwrap();
        let error = store.source_states(&"pi".into()).unwrap_err().to_string();
        assert!(!error.contains("PRIVATE_TEXT"));
    }
}
