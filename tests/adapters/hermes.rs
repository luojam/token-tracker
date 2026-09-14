use rusqlite::Connection;
use token_tracker::adapters::hermes::{HermesReadError, read_snapshot};
use token_tracker::application::SnapshotCompletion;
use token_tracker::domain::{ParentSession, TokenCounts, UsageKind};

use crate::support::TempTree;

fn fixture(tree: &TempTree) -> (std::path::PathBuf, Connection) {
    let path = tree.root.join("state.db");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY, started_at REAL NOT NULL,
            input_tokens INTEGER DEFAULT 0, output_tokens INTEGER DEFAULT 0,
            cache_read_tokens INTEGER DEFAULT 0, cache_write_tokens INTEGER DEFAULT 0,
            reasoning_tokens INTEGER DEFAULT 0, api_call_count INTEGER DEFAULT 0,
            cwd TEXT, title TEXT, parent_session_id TEXT,
            model_config BLOB, hidden INTEGER DEFAULT 0, archived INTEGER DEFAULT 0
        );
        CREATE TABLE session_model_usage (
            session_id TEXT NOT NULL, model TEXT NOT NULL,
            billing_provider TEXT NOT NULL DEFAULT '', billing_base_url TEXT NOT NULL DEFAULT '',
            billing_mode TEXT NOT NULL DEFAULT '', task TEXT NOT NULL DEFAULT '',
            input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0, cache_write_tokens INTEGER NOT NULL DEFAULT 0,
            reasoning_tokens INTEGER NOT NULL DEFAULT 0, api_call_count INTEGER NOT NULL DEFAULT 0,
            first_seen REAL, last_seen REAL,
            actual_cost_usd REAL DEFAULT 0, estimated_cost_usd REAL DEFAULT 0,
            cost_status TEXT, cost_source TEXT, extra_column BLOB,
            PRIMARY KEY (session_id, model, billing_provider, billing_base_url, billing_mode, task)
        );
        INSERT INTO sessions (id, started_at, input_tokens, output_tokens,
            cache_read_tokens, cache_write_tokens, reasoning_tokens, api_call_count,
            cwd, title, parent_session_id, hidden, archived)
        VALUES ('child', 1700000000.125, 160, 55, 600, 30, 45, 12,
            '/workspace', 'Synthetic session', 'parent', 1, 1);
        INSERT INTO sessions (id, started_at, cwd) VALUES ('empty', 1700000001, 'relative');
        INSERT INTO session_model_usage (session_id, model, billing_provider, billing_mode,
            task, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
            reasoning_tokens, api_call_count, first_seen)
        VALUES
            ('child', 'model-a', 'openai-codex', 'subscription', '', 100, 20, 400, 10, 15, 5, NULL),
            ('child', 'model-b', 'openai-codex', 'subscription', '', 40, 35, 250, 20, 30, 7, 1700000002.5),
            ('child', 'model-a', 'auto', '', 'compression', 5, 3, 50, 2, 2, 1, 1700000003),
            ('child', 'model-a', 'auto', '', 'title_generation', 7, 2, 10, 0, 1, 1, 1700000004);"
    ).unwrap();
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
        ["hermes_unattributed_residual", "hermes_counter_mismatch"]
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
