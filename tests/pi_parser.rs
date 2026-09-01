use std::io::Cursor;
use std::path::PathBuf;

use token_tracker::adapters::pi::{PiParseError, PiSessionParser};
use token_tracker::application::{ParseCompletion, ParsedSession, SessionParser};
use token_tracker::core::{
    AgentId, ModelAttribution, RecordedCost, SessionMetadata, Timestamp, TokenCounts, UsageEvent,
    UsageEventIdentity, UsageKind,
};

const ALL_USAGE: &str = include_str!("fixtures/pi/all-usage.jsonl");
const INCOMPLETE_FINAL_LINE: &str = include_str!("fixtures/pi/incomplete-final-line.jsonl");
const MALFORMED_COMPLETE_LINE: &str = include_str!("fixtures/pi/malformed-complete-line.jsonl");

fn parse(source: &str) -> Result<ParsedSession, PiParseError> {
    parse_bytes(source.as_bytes())
}

fn parse_bytes(source: &[u8]) -> Result<ParsedSession, PiParseError> {
    PiSessionParser::new().parse(&mut Cursor::new(source))
}

fn cost(value: f64) -> Option<RecordedCost> {
    Some(RecordedCost::from_usd(value).unwrap())
}

#[test]
fn parses_every_usage_location_without_exposing_session_content() {
    let parsed = parse(ALL_USAGE).unwrap();

    assert_eq!(
        parsed.metadata,
        SessionMetadata {
            agent: AgentId::from("pi"),
            session_id: "01940000-0000-7000-8000-000000000001".into(),
            format_version: 3,
            working_directory: PathBuf::from("/work/project"),
            started_at: Timestamp::from_unix_milliseconds(1_735_787_045_006),
            name: Some("Fixture session".into()),
            parent_session: Some("/sessions/original.jsonl".into()),
        }
    );
    assert_eq!(parsed.completion, ParseCompletion::Complete);
    assert_eq!(
        parsed.events,
        vec![
            UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::from("pi"),
                    adapter_key: "v1:assistant:1735787100100:a1a1a1a1".into(),
                },
                timestamp: Timestamp::from_unix_milliseconds(1_735_787_100_100),
                kind: UsageKind::Assistant,
                attribution: Some(ModelAttribution {
                    provider: "provider-a".into(),
                    model: "model-resolved".into(),
                }),
                tokens: TokenCounts {
                    input: 10,
                    output: 20,
                    cache_read: 30,
                    cache_write: 40,
                },
                recorded_cost: cost(0.12),
            },
            UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::from("pi"),
                    adapter_key: "v1:tool-result:1735787160200:b2b2b2b2".into(),
                },
                timestamp: Timestamp::from_unix_milliseconds(1_735_787_160_200),
                kind: UsageKind::ToolResult,
                attribution: None,
                tokens: TokenCounts {
                    input: 1,
                    output: 2,
                    cache_read: 3,
                    cache_write: 4,
                },
                recorded_cost: None,
            },
            UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::from("pi"),
                    adapter_key: "v1:compaction:1735787220300:c3c3c3c3".into(),
                },
                timestamp: Timestamp::from_unix_milliseconds(1_735_787_220_300),
                kind: UsageKind::Compaction,
                attribution: None,
                tokens: TokenCounts {
                    input: 5,
                    output: 6,
                    cache_read: 7,
                    cache_write: 8,
                },
                recorded_cost: cost(0.34),
            },
            UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::from("pi"),
                    adapter_key: "v1:branch-summary:1735787280400:d4d4d4d4".into(),
                },
                timestamp: Timestamp::from_unix_milliseconds(1_735_787_280_400),
                kind: UsageKind::BranchSummary,
                attribution: None,
                tokens: TokenCounts {
                    input: 9,
                    output: 10,
                    cache_read: 11,
                    cache_write: 12,
                },
                recorded_cost: cost(0.56),
            },
        ]
    );

    let output = format!("{parsed:?}");
    for private_content in [
        "SECRET_PROMPT",
        "SECRET_THINKING",
        "SECRET_RESPONSE",
        "SECRET_TOOL_OUTPUT",
        "SECRET_COMPACTION_SUMMARY",
        "SECRET_BRANCH_SUMMARY",
        "SECRET_DETAILS",
        "SECRET_FUTURE_CONTENT",
        "SECRET_FUTURE_MESSAGE",
    ] {
        assert!(!output.contains(private_content));
    }

    // Session IDs and mutable usage values do not participate in copied-entry keys.
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
    assert_eq!(parsed.completion, ParseCompletion::IncompleteFinalLine);
    assert_eq!(parsed.events.len(), 1);

    let error = parse(MALFORMED_COMPLETE_LINE).unwrap_err();
    assert!(matches!(
        &error,
        PiParseError::MalformedLine { line: 3, .. }
    ));
    let rendered_error = format!("{error:?} {error}");
    assert!(!rendered_error.contains("SECRET_MALFORMED_CONTENT"));

    let final_line_start = INCOMPLETE_FINAL_LINE.rfind('\n').unwrap() + 1;
    let mut split_utf8 = INCOMPLETE_FINAL_LINE.as_bytes()[..final_line_start].to_vec();
    split_utf8
        .extend_from_slice(b"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"");
    split_utf8.extend_from_slice(&[0xf0, 0x9f]);
    let parsed = parse_bytes(&split_utf8).unwrap();
    assert_eq!(parsed.completion, ParseCompletion::IncompleteFinalLine);
    assert_eq!(parsed.events.len(), 1);
}
