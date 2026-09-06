use std::path::PathBuf;

use token_tracker::application::{
    ImportWarning, SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot,
    render_terminal_report, summarize_usage,
};
use token_tracker::core::{
    AgentId, ModelAttribution, ParentSession, RecordedCost, Timestamp, TokenCounts, UsageEvent,
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

fn session(
    path: &str,
    session_id: &str,
    started_at: i64,
    parent_session: Option<&str>,
    events: Vec<UsageEvent>,
) -> (SessionProvenance, Vec<UsageObservation>) {
    let key = SourceSessionKey {
        agent: "pi".into(),
        session_id: session_id.into(),
        source_path: path.into(),
    };
    let provenance = SessionProvenance {
        key: key.clone(),
        started_at: Timestamp::from_unix_milliseconds(started_at),
        parent_session: parent_session.map(|path| ParentSession::SourcePath(path.into())),
    };
    (
        provenance,
        events
            .into_iter()
            .map(|event| UsageObservation {
                session: key.clone(),
                event,
            })
            .collect(),
    )
}

fn sessions() -> [(SessionProvenance, Vec<UsageObservation>); 2] {
    let original = session(
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
    let child = session(
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

fn snapshot(reverse: bool) -> UsageSnapshot {
    let mut records = sessions();
    if reverse {
        records.reverse();
    }
    let mut snapshot = UsageSnapshot::default();
    for (session, observations) in records {
        snapshot.sessions.push(session);
        snapshot.observations.extend(observations);
    }
    snapshot
}

fn summary_for_order(reverse: bool) -> token_tracker::core::UsageSummary {
    summarize_usage(&snapshot(reverse)).unwrap()
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

#[test]
fn typed_lineage_and_ambiguous_provenance_use_deterministic_precedence() {
    let mut data = snapshot(false);
    let original = data.sessions[0].key.clone();
    let child = data.sessions[1].key.clone();
    data.sessions[1].parent_session = Some(ParentSession::SessionId(original.session_id.clone()));
    assert_eq!(summarize_usage(&data).unwrap(), summary_for_order(false));

    // Cycles are treated as ambiguous, never as an import-order tie breaker.
    data.sessions[0].parent_session = Some(ParentSession::SessionId(child.session_id));
    let expected = summarize_usage(&data).unwrap();
    assert_eq!(expected.totals.tokens.input, 1005);
    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(summarize_usage(&data).unwrap(), expected);
}
