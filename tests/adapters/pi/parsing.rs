use std::io::Cursor;
use std::path::{Path, PathBuf};
use token_tracker::adapters::files::{ParseContext, SessionParser};

use token_tracker::adapters::pi::{PiParseError, PiSessionParser};
use token_tracker::application::{SessionData, SnapshotCompletion};
use token_tracker::domain::{
    AgentId, ModelAttribution, ParentSession, RecordedCost, SessionMetadata, Timestamp,
    TokenCounts, UsageEvent, UsageEventIdentity, UsageKind,
};

const ALL_USAGE: &str = include_str!("../../fixtures/pi/all-usage.jsonl");
const INCOMPLETE_FINAL_LINE: &str = include_str!("../../fixtures/pi/incomplete-final-line.jsonl");
const MALFORMED_COMPLETE_LINE: &str =
    include_str!("../../fixtures/pi/malformed-complete-line.jsonl");

fn parse(source: &str) -> Result<SessionData, PiParseError> {
    parse_bytes(source.as_bytes())
}

fn parse_bytes(source: &[u8]) -> Result<SessionData, PiParseError> {
    PiSessionParser::new().parse(
        &mut Cursor::new(source),
        ParseContext {
            source_path: Path::new("/sessions/fixture.jsonl"),
        },
    )
}

#[test]
fn parses_every_usage_location_without_exposing_session_content() {
    let parsed = parse(ALL_USAGE).unwrap();

    assert_eq!(
        parsed.metadata,
        SessionMetadata {
            agent: AgentId::from("pi"),
            session_id: "01940000-0000-7000-8000-000000000001".into(),

            working_directory: Some(PathBuf::from("/work/project")),
            started_at: Timestamp::from_unix_milliseconds(1_735_787_045_006),
            name: Some("Fixture session".into()),
            parent_session: Some(ParentSession::SourcePath("/sessions/original.jsonl".into())),
        }
    );
    assert_eq!(parsed.completion, SnapshotCompletion::Complete);
    let expected = [
        (
            "v1:assistant:1735787100100:a1a1a1a1",
            1_735_787_100_100,
            UsageKind::Assistant,
            Some(("provider-a", "model-resolved")),
            [10, 30, 40, 20],
            Some(0.12),
        ),
        (
            "v1:tool-result:1735787160200:b2b2b2b2",
            1_735_787_160_200,
            UsageKind::ToolResult,
            None,
            [1, 3, 4, 2],
            None,
        ),
        (
            "v1:compaction:1735787220300:c3c3c3c3",
            1_735_787_220_300,
            UsageKind::Compaction,
            None,
            [5, 7, 8, 6],
            Some(0.34),
        ),
        (
            "v1:branch-summary:1735787280400:d4d4d4d4",
            1_735_787_280_400,
            UsageKind::BranchSummary,
            None,
            [9, 11, 12, 10],
            Some(0.56),
        ),
    ]
    .map(
        |(key, time, kind, attribution, [input, cache_read, cache_write, output], cost)| {
            UsageEvent {
                identity: UsageEventIdentity {
                    agent: "pi".into(),
                    adapter_key: key.into(),
                },
                timestamp: Timestamp::from_unix_milliseconds(time),
                kind,
                attribution: attribution.map(|(provider, model)| ModelAttribution {
                    provider: provider.into(),
                    model: model.into(),
                }),
                tokens: TokenCounts {
                    input,
                    cache_read,
                    cache_write,
                    output,
                },
                recorded_cost: cost.map(|value| RecordedCost::from_usd(value).unwrap()),
                pricing_context: None,
            }
        },
    );
    assert_eq!(parsed.events, expected);
    assert!(!format!("{parsed:?}").contains("SECRET_"));

    let copied = ALL_USAGE
        .replacen(
            "01940000-0000-7000-8000-000000000001",
            "01940000-0000-7000-8000-000000000999",
            1,
        )
        .replacen(
            "\"input\":10,\"output\":20",
            "\"input\":999,\"output\":20",
            1,
        );
    let copied = parse(&copied).unwrap();
    assert_eq!(copied.events[0].tokens.input, 999);
    assert_eq!(
        parsed
            .events
            .iter()
            .map(|event| &event.identity)
            .collect::<Vec<_>>(),
        copied
            .events
            .iter()
            .map(|event| &event.identity)
            .collect::<Vec<_>>()
    );
}

#[test]
fn distinguishes_an_incomplete_final_line_from_a_malformed_complete_line() {
    let parsed = parse(INCOMPLETE_FINAL_LINE).unwrap();
    assert_eq!(parsed.completion, SnapshotCompletion::Partial);
    assert_eq!(parsed.events.len(), 1);

    let error = parse(MALFORMED_COMPLETE_LINE).unwrap_err();
    assert!(matches!(
        &error,
        PiParseError::MalformedLine { line: 3, .. }
    ));
    let rendered_error = format!("{error:?} {error}");
    assert!(!rendered_error.contains("SECRET_MALFORMED_CONTENT"));
}
