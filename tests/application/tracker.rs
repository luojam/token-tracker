use std::fs;

use rusqlite::Connection;
use token_tracker::{
    ImportSynchronizationError, LocalSourceConfig, ReportError, TokenTracker, TokenTrackerConfig,
};

use super::adapters::pi_session;
use crate::support::{TempTree, fixture};

#[test]
fn report_reads_stored_usage_and_notices_without_refreshing_or_changing_state() {
    let tree = TempTree::new();
    let database = tree.root.join("usage.db");
    let pi = tree.write("pi/session.jsonl", pi_session(7));
    let claude = tree.write(
        "claude/project/11111111-1111-4111-8111-111111111111.jsonl",
        fixture("claude", "partial-ignored.jsonl"),
    );
    let mut tracker = TokenTracker::open(TokenTrackerConfig {
        database_path: Some(database.clone()),
        sources: vec![
            LocalSourceConfig::Pi {
                root: Some(tree.root.join("pi")),
            },
            LocalSourceConfig::Claude {
                root: Some(tree.root.join("claude")),
            },
        ],
        ..Default::default()
    })
    .unwrap();
    assert_eq!(tracker.report().unwrap().report.totals.session_count, 0);

    let imported = tracker.refresh().unwrap();
    assert_eq!(imported.counts.sources_imported, 2);
    assert_eq!(imported.counts.event_identities_inserted, 2);
    assert!(imported.warnings.is_empty());

    let expected = tracker.report().unwrap();
    assert_eq!(expected.report.totals.tokens.input, 9);
    assert_eq!(expected.diagnostics.len(), 1);
    assert_eq!(expected.diagnostics[0].agent.as_str(), "claude");
    assert_eq!(expected.diagnostics[0].notice.count.get(), 2);

    fs::write(pi, pi_session(100)).unwrap();
    fs::remove_file(claude).unwrap();
    let stored = fs::read(&database).unwrap();
    assert_eq!(tracker.report().unwrap(), expected);
    assert_eq!(fs::read(&database).unwrap(), stored);
    drop(tracker);

    for sources in [
        vec![],
        vec![LocalSourceConfig::Claude {
            root: Some("".into()),
        }],
    ] {
        let expected_warning_count = sources.len();
        let mut tracker = TokenTracker::open(TokenTrackerConfig {
            database_path: Some(database.clone()),
            sources,
            ..Default::default()
        })
        .unwrap();
        let stored = fs::read(&database).unwrap();
        assert_eq!(tracker.report().unwrap(), expected);
        assert_eq!(fs::read(&database).unwrap(), stored);
        let refreshed = tracker.refresh().unwrap();
        assert_eq!(refreshed.warnings.len(), expected_warning_count);
        if expected_warning_count > 0 {
            assert!(
                refreshed.warnings[0]
                    .message
                    .starts_with("claude: session discovery failed:")
            );
        }
        assert_eq!(tracker.report().unwrap(), expected);
    }
}

#[test]
fn refresh_recovers_from_source_failures_but_storage_failures_are_fatal() {
    let tree = TempTree::new();
    let database = tree.root.join("usage.db");
    tree.write("pi/valid.jsonl", pi_session(7));
    tree.write("pi/invalid.jsonl", "not json\n");
    let mut tracker = TokenTracker::open(TokenTrackerConfig {
        database_path: Some(database.clone()),
        sources: vec![
            LocalSourceConfig::Codex {
                roots: Some(vec!["".into()]),
            },
            LocalSourceConfig::Pi {
                root: Some(tree.root.join("pi")),
            },
        ],
        ..Default::default()
    })
    .unwrap();

    let imported = tracker.refresh().unwrap();
    assert_eq!(imported.counts.sources_imported, 1);
    assert_eq!(imported.counts.sources_failed, 1);
    assert_eq!(imported.warnings.len(), 2);
    assert!(
        imported.warnings[0]
            .message
            .starts_with("codex: session discovery failed:")
    );
    assert_eq!(tracker.report().unwrap().report.totals.tokens.input, 7);

    Connection::open(database)
        .unwrap()
        .execute_batch("PRAGMA foreign_keys = OFF; DROP TABLE import_sources;")
        .unwrap();

    assert!(matches!(
        tracker.refresh(),
        Err(ImportSynchronizationError::Storage { .. })
    ));
    assert!(matches!(tracker.report(), Err(ReportError::Storage(_))));
}
