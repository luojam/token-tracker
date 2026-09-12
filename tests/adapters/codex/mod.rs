use token_tracker::adapters::files::{ParseContext, SessionParser};
mod cache;
mod context;
mod discovery;
mod legacy;
mod mirrors;
mod parsing;
mod responses;
mod review;

use crate::support::{fixture, jsonl};
use serde_json::{Value, json};
use std::io::{BufReader, Cursor};
use std::path::Path;
use token_tracker::adapters::codex::{CodexParseError, CodexSessionParser};
use token_tracker::application::{SessionData, SnapshotCompletion};
use token_tracker::domain::{TokenCounts, UsageKind};

fn parse(source: &str) -> Result<SessionData, CodexParseError> {
    parse_bytes(source.as_bytes())
}

fn parse_bytes(source: &[u8]) -> Result<SessionData, CodexParseError> {
    CodexSessionParser::new().parse(
        &mut BufReader::with_capacity(1, Cursor::new(source)),
        ParseContext {
            source_path: Path::new("/not-read/rollout-fixture.jsonl"),
        },
    )
}

fn parse_records(records: &[Value]) -> Result<SessionData, CodexParseError> {
    parse(&jsonl(records))
}

fn tokens(counts: TokenCounts) -> Value {
    json!([
        counts.input,
        counts.cache_read,
        counts.cache_write,
        counts.output
    ])
}

#[test]
fn fixtures_and_prefixes_preserve_accounting() {
    let expected: Value =
        serde_json::from_str(include_str!("../../fixtures/codex/expectations.json")).unwrap();
    for (name, expected) in expected["fixtures"].as_object().unwrap() {
        let result = parse(&fixture("codex", name));
        if !expected["reject"].is_null() {
            assert!(
                matches!(result, Err(CodexParseError::InvalidField { .. })),
                "{name}: {result:?}"
            );
            continue;
        }
        let parsed = result.unwrap_or_else(|error| panic!("{name}: {error}"));
        let actual: Vec<_> = parsed
            .events
            .iter()
            .map(|event| {
                assert_eq!(event.identity.agent.as_str(), "codex", "{name}");
                assert_eq!(event.kind, UsageKind::Other, "{name}");
                assert_eq!(event.recorded_cost, None, "{name}");
                json!({"key": event.identity.adapter_key, "tokens": tokens(event.tokens)})
            })
            .collect();
        assert_eq!(json!(actual), expected["events"], "{name}");
        assert_eq!(
            parsed.completion == SnapshotCompletion::Complete,
            expected["completion"] == "complete",
            "{name}"
        );
    }
    for (name, prefixes) in expected["prefixes"].as_object().unwrap() {
        let source = fixture("codex", name);
        for (lines, expected) in prefixes.as_object().unwrap() {
            let result = parse(&crate::support::prefix(&source, lines.parse().unwrap()));
            if expected.is_array() {
                let actual: Vec<_> = result
                    .unwrap()
                    .events
                    .iter()
                    .map(|e| json!([e.identity.adapter_key, tokens(e.tokens)]))
                    .collect();
                assert_eq!(json!(actual), *expected, "{name}:{lines}");
            } else {
                assert!(result.is_err(), "{name}:{lines}");
            }
        }
    }
}
