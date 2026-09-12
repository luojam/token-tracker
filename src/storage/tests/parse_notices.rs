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
    store.commit_import(&validated(&original)).unwrap();
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
    assert!(store.commit_import(&validated(&replacement)).is_err());
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
    assert_eq!(store.usage_snapshot().unwrap(), before);

    replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_000_000);
    assert_eq!(
        store.commit_import(&validated(&replacement)).unwrap(),
        CommitImportOutcome::IgnoredStale
    );
    assert_eq!(store.source_states(&"pi".into()).unwrap(), states);
}

#[test]
fn corrupt_notice_data_is_rejected_without_exposing_contents() {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let import = session_import("/sessions/a.jsonl", 10);
    store.commit_import(&validated(&import)).unwrap();
    let valid = serde_json::to_value(notice()).unwrap();
    let invalid_field = |field: &str, value: serde_json::Value| {
        let mut invalid = valid.clone();
        invalid[field] = value;
        serde_json::json!([invalid])
    };
    for invalid in [
        invalid_field("count", serde_json::json!(0)),
        invalid_field("line", serde_json::json!(0)),
        invalid_field("text", serde_json::json!("PRIVATE_TEXT")),
        serde_json::json!([valid.clone(), valid.clone()]),
    ] {
        store
            .connection
            .execute(
                "UPDATE import_sources SET parse_notices = ?1",
                [invalid.to_string()],
            )
            .unwrap();
        let error = store.source_states(&"pi".into()).unwrap_err().to_string();
        assert!(!error.contains("PRIVATE_TEXT"));
    }
}
