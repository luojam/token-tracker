use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use token_tracker::adapters::sqlite::SqliteUsageStore;
use token_tracker::application::{
    DiscoveredSessionFile, FileRevision, ImportWarning, ParseCompletion, ParsedSession,
    SessionImport, UsageStore, UsageSummaryStore, render_terminal_report,
};
use token_tracker::core::{
    AgentId, ModelAttribution, RecordedCost, SessionMetadata, Timestamp, TokenCounts, UsageEvent,
    UsageEventIdentity, UsageKind,
};

fn event(
    key: &str,
    kind: UsageKind,
    attribution: Option<(&str, &str)>,
    tokens: TokenCounts,
    cost: Option<f64>,
) -> UsageEvent {
    UsageEvent {
        identity: UsageEventIdentity {
            agent: AgentId::from("pi"),
            adapter_key: key.into(),
        },
        timestamp: Timestamp::from_unix_milliseconds(1_000),
        kind,
        attribution: attribution.map(|(provider, model)| ModelAttribution {
            provider: provider.into(),
            model: model.into(),
        }),
        tokens,
        recorded_cost: cost.map(|value| RecordedCost::from_usd(value).unwrap()),
    }
}

fn session_import(
    path: &str,
    session_id: &str,
    started_at: i64,
    parent_session: Option<&str>,
    events: Vec<UsageEvent>,
) -> SessionImport {
    SessionImport {
        source: DiscoveredSessionFile {
            path: PathBuf::from(path),
            revision: FileRevision {
                size: 100,
                modified_at: UNIX_EPOCH + Duration::from_secs(10),
            },
        },
        scanned_at: Timestamp::from_unix_milliseconds(2_000),
        parsed: ParsedSession {
            metadata: SessionMetadata {
                agent: AgentId::from("pi"),
                session_id: session_id.into(),
                format_version: 3,
                working_directory: PathBuf::from("/work/project"),
                started_at: Timestamp::from_unix_milliseconds(started_at),
                name: None,
                parent_session: parent_session.map(Into::into),
            },
            events,
            completion: ParseCompletion::Complete,
        },
    }
}

fn imports() -> [SessionImport; 2] {
    let original = session_import(
        "/sessions/original.jsonl",
        "original-session",
        200,
        None,
        vec![
            event(
                "shared",
                UsageKind::Assistant,
                Some(("provider-a", "model-a")),
                TokenCounts {
                    input: 10,
                    output: 2,
                    cache_read: 3,
                    cache_write: 4,
                },
                Some(0.25),
            ),
            event(
                "tool",
                UsageKind::ToolResult,
                None,
                TokenCounts {
                    input: 1,
                    output: 1,
                    cache_read: 1,
                    cache_write: 1,
                },
                None,
            ),
        ],
    );
    let child = session_import(
        "/sessions/child.jsonl",
        "child-session",
        100,
        Some("/sessions/original.jsonl"),
        vec![
            event(
                "shared",
                UsageKind::Assistant,
                Some(("wrong-provider", "wrong-model")),
                TokenCounts {
                    input: 999,
                    output: 999,
                    cache_read: 999,
                    cache_write: 999,
                },
                Some(9.99),
            ),
            event(
                "branch",
                UsageKind::BranchSummary,
                None,
                TokenCounts {
                    input: 5,
                    output: 6,
                    cache_read: 7,
                    cache_write: 8,
                },
                Some(0.5),
            ),
        ],
    );
    [original, child]
}

fn summary_for_order(reverse: bool) -> token_tracker::core::UsageSummary {
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let [original, child] = imports();
    if reverse {
        store.commit_import(&child).unwrap();
        store.commit_import(&original).unwrap();
    } else {
        store.commit_import(&original).unwrap();
        store.commit_import(&child).unwrap();
    }
    store.all_time_summary().unwrap()
}

#[test]
fn summary_reconciles_and_renders_independently_of_observation_order() {
    let summary = summary_for_order(false);
    assert_eq!(summary, summary_for_order(true));
    assert_eq!(
        render_terminal_report(
            &summary,
            &[
                ImportWarning {
                    path: Some(PathBuf::from("/sessions/z-bad.jsonl")),
                    message: "could not parse".into(),
                },
                ImportWarning {
                    path: None,
                    message: "discovery warning".into(),
                },
            ],
        ),
        "Token Tracker — All Time\n\
         \n\
         Input tokens: 16\n\
         Output tokens: 9\n\
         Cache-read tokens: 11\n\
         Cache-write tokens: 13\n\
         Total tokens: 49\n\
         Recorded cost: $0.750000\n\
         Sessions: 2\n\
         Unique usage events: 3\n\
         \n\
         Usage by provider/model:\n\
         - provider-a / model-a: input 10, output 2, cache read 3, cache write 4, total 19, events 1, cost $0.250000\n\
         - Unattributed tool results: input 1, output 1, cache read 1, cache write 1, total 4, events 1\n\
         - Unattributed branch summaries: input 5, output 6, cache read 7, cache write 8, total 26, events 1, cost $0.500000\n\
         \n\
         Warnings (2):\n\
         - discovery warning\n\
         - /sessions/z-bad.jsonl: could not parse\n"
    );
}
