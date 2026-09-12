use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::Path;
use token_tracker::adapters::files::{ParseContext, SessionParser};

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use token_tracker::adapters::claude::{ClaudeParseError, ClaudeSessionParser};
use token_tracker::application::{SessionData, SnapshotCompletion};
use token_tracker::domain::{ParentSession, Timestamp, TokenCounts, UsageKind};

const SOURCE_PATH: &str =
    "/invented/claude/projects/fixture/11111111-1111-4111-8111-111111111111.jsonl";
const SNAPSHOTS: &str = include_str!("../../fixtures/claude/snapshots.jsonl");
const CHILD: &str = include_str!("../../fixtures/claude/child-a1b2c3d.jsonl");

fn parse(source: &[u8], path: &str) -> Result<SessionData, ClaudeParseError> {
    ClaudeSessionParser::new().parse(
        &mut BufReader::with_capacity(1, Cursor::new(source)),
        ParseContext {
            source_path: Path::new(path),
        },
    )
}

fn oracle() -> Value {
    serde_json::from_str(include_str!("../../fixtures/claude/expectations.json")).unwrap()
}

fn time(timestamp: Timestamp) -> String {
    DateTime::<Utc>::from_timestamp_millis(timestamp.as_unix_milliseconds())
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn tokens(tokens: TokenCounts) -> Value {
    json!([
        tokens.input,
        tokens.cache_read,
        tokens.cache_write,
        tokens.output
    ])
}

fn check_usage(parsed: &SessionData, expected: &Value, name: &str) {
    let actual: BTreeMap<_, _> = parsed
        .events
        .iter()
        .map(|event| {
            assert_eq!(event.identity.agent.as_str(), "claude", "{name}");
            assert_eq!(event.kind, UsageKind::Assistant, "{name}");
            assert_eq!(event.recorded_cost, None, "{name}");
            (
                event.identity.adapter_key.clone(),
                json!({
                    "key": event.identity.adapter_key,
                    "timestamp": time(event.timestamp),
                    "kind": "assistant",
                    "attribution": event.attribution.as_ref().map(|attribution| json!({
                        "provider": attribution.provider,
                        "model": attribution.model,
                    })),
                    "tokens": tokens(event.tokens),
                    "recorded_cost": null,
                    "pricing_facts": event.pricing_context.as_ref().map(|context| match context {
                        token_tracker::domain::PricingContext::Anthropic(facts) => serde_json::to_value(facts).unwrap(),
                        _ => panic!("expected Anthropic billing"),
                    }),
                }),
            )
        })
        .collect();
    let events: BTreeMap<_, _> = expected["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| (event["key"].as_str().unwrap().to_owned(), event.clone()))
        .collect();
    assert_eq!(actual, events, "{name}");
    assert_eq!(parsed.events.len(), actual.len(), "{name}");
    let notices: BTreeMap<_, _> = parsed
        .notices
        .iter()
        .map(|notice| {
            assert!(notice.line.is_some(), "{name}");
            (notice.code.clone(), notice.count.get())
        })
        .collect();
    let expected_notices: BTreeMap<_, _> = expected["notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|notice| {
            (
                notice["code"].as_str().unwrap().to_owned(),
                notice["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(notices, expected_notices, "{name}");
    assert_eq!(parsed.notices.len(), notices.len(), "{name}");
}

#[test]
fn fixtures_preserve_metadata_usage_and_errors() {
    for (name, expected) in oracle()["fixtures"].as_object().unwrap() {
        let source = fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/claude")
                .join(name),
        )
        .unwrap();
        let result = parse(&source, expected["source_path"].as_str().unwrap());
        if !expected["error"].is_null() {
            let (line, reason) = match result.unwrap_err() {
                ClaudeParseError::MalformedLine { line } => (line, "invalid_json"),
                ClaudeParseError::InvalidField { line, field } => (
                    line,
                    match field {
                        "conflicting model" => "conflicting_model",
                        "conflicting request id" => "conflicting_request_id",
                        "cache duration sum mismatch" => "cache_duration_sum_mismatch",
                        "required counter" => "invalid_required_counter",
                        "non-compaction total mismatch" => "non_compaction_total_mismatch",
                        _ => panic!("{name}: unexpected field {field}"),
                    },
                ),
                error => panic!("{name}: unexpected error {error}"),
            };
            assert_eq!(json!(line), expected["error"]["line"], "{name}");
            assert_eq!(json!(reason), expected["error"]["reason"], "{name}");
            continue;
        }
        let parsed = result.unwrap_or_else(|error| panic!("{name}: {error}"));
        let metadata = &parsed.metadata;
        assert_eq!(
            json!({
                "agent": metadata.agent.as_str(),
                "session_id": metadata.session_id,
                "working_directory": metadata.working_directory,
                "started_at": time(metadata.started_at),
                "name": metadata.name,
                "parent_session": metadata.parent_session.as_ref().map(|parent| match parent {
                    ParentSession::SessionId(id) => json!({"session_id": id}),
                    ParentSession::SourcePath(_) => panic!("unexpected parent path"),
                }),
            }),
            expected["session"],
            "{name}"
        );
        assert_eq!(
            match parsed.completion {
                SnapshotCompletion::Complete => "complete",
                SnapshotCompletion::Partial => "incomplete_final_line",
            },
            expected["completion"],
            "{name}"
        );
        check_usage(&parsed, expected, name);
    }
}

#[test]
fn snapshot_prefixes_keep_finals_and_first_final_timestamps() {
    for expected in oracle()["prefixes"].as_array().unwrap() {
        let count = expected["complete_lines"].as_u64().unwrap() as usize;
        let source = SNAPSHOTS.lines().take(count).collect::<Vec<_>>().join("\n");
        let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
        assert_eq!(parsed.completion, SnapshotCompletion::Complete);
        check_usage(&parsed, expected, &format!("prefix {count}"));
    }
}

fn final_record() -> Value {
    serde_json::from_str(SNAPSHOTS.lines().nth(3).unwrap()).unwrap()
}

fn parse_record(record: &Value) -> Result<SessionData, ClaudeParseError> {
    parse(record.to_string().as_bytes(), SOURCE_PATH)
}

#[test]
fn iteration_models_use_the_model_from_earlier_snapshots() {
    let mut original = final_record();
    let mut iteration = original["message"]["usage"].clone();
    iteration["type"] = json!("message");
    iteration["model"] = original["message"]["model"].clone();
    original["message"]["usage"]["iterations"] = json!([iteration]);
    let expected = parse_record(&original).unwrap();
    assert_eq!(expected.events.len(), 1);

    let mut final_snapshot = original.clone();
    final_snapshot["message"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    for stop_reason in [original["message"]["stop_reason"].clone(), Value::Null] {
        let mut earlier = original.clone();
        earlier["message"]["stop_reason"] = stop_reason;
        let source = format!("{earlier}\n{final_snapshot}");
        assert_eq!(parse(source.as_bytes(), SOURCE_PATH).unwrap(), expected);
    }

    final_snapshot["message"]["usage"]["iterations"][0]["model"] = json!("different-model");
    let source = format!("{original}\n{final_snapshot}");
    let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
    assert!(parsed.events.is_empty());
    assert_eq!(parsed.notices.len(), 1);
    assert_eq!(parsed.notices[0].code, "unsupported_response_accounting");
}

#[test]
fn final_usage_is_revalidated_when_a_placeholder_supplies_the_model() {
    let mut original = final_record();
    let mut iteration = original["message"]["usage"].clone();
    iteration["type"] = json!("message");
    iteration["model"] = original["message"]["model"].clone();
    original["message"]["usage"]["iterations"] = json!([iteration]);
    let expected = parse_record(&original).unwrap();
    assert_eq!(expected.events.len(), 1);

    let mut final_snapshot = original.clone();
    final_snapshot["message"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    let mut placeholder = original.clone();
    placeholder["timestamp"] = json!("2026-01-01T00:00:06Z");
    placeholder["message"]["stop_reason"] = Value::Null;
    placeholder["message"]["usage"] = json!({
        "input_tokens": 999,
        "output_tokens": 0,
        "cache_read_input_tokens": 999,
        "cache_creation_input_tokens": 0,
    });
    for source in [
        format!("{placeholder}\n{final_snapshot}"),
        format!("{final_snapshot}\n{placeholder}"),
    ] {
        assert_eq!(parse(source.as_bytes(), SOURCE_PATH).unwrap(), expected);
    }

    placeholder["message"]["model"] = json!("different-model");
    let source = format!("{final_snapshot}\n{placeholder}");
    let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
    assert!(parsed.events.is_empty());
    assert_eq!(parsed.notices.len(), 1);
    assert_eq!(parsed.notices[0].code, "unsupported_response_accounting");
    assert_eq!(parsed.notices[0].line.unwrap().get(), 1);
}

#[test]
fn validates_source_identity_and_requires_record_metadata() {
    for (field, value) in [
        ("sessionId", json!("22222222-2222-4222-8222-222222222222")),
        ("sessionId", json!(42)),
        ("agentId", json!("child")),
        ("timestamp", json!("PRIVATE_INVALID_TIMESTAMP")),
    ] {
        let mut record = final_record();
        record[field] = value;
        let error = parse_record(&record).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
    let mut record = final_record();
    record.as_object_mut().unwrap().remove("sessionId");
    assert!(matches!(
        parse_record(&record),
        Err(ClaudeParseError::MissingMetadata { field: "sessionId" })
    ));
    record = json!({"sessionId": "11111111-1111-4111-8111-111111111111", "session_id": "ignored"});
    assert!(matches!(
        parse_record(&record),
        Err(ClaudeParseError::MissingMetadata { field: "timestamp" })
    ));
    assert!(parse(b"", SOURCE_PATH).is_err());

    let child_path = "/invented/project/11111111-1111-4111-8111-111111111111/subagents/nested/agent-a1b2c3d.jsonl";
    assert!(parse(CHILD.as_bytes(), child_path).is_ok());
    let mut child: Value = serde_json::from_str(CHILD).unwrap();
    child.as_object_mut().unwrap().remove("agentId");
    assert!(matches!(
        parse(child.to_string().as_bytes(), child_path),
        Err(ClaudeParseError::MissingMetadata { field: "agentId" })
    ));
    child["agentId"] = json!("different");
    assert!(parse(child.to_string().as_bytes(), child_path).is_err());
    for path in [
        "relative.jsonl",
        "/project/not-a-session.jsonl",
        "/project/agent-a.jsonl",
        "/project/11111111-1111-4111-8111-111111111111/subagents/agent-a:b.jsonl",
    ] {
        assert!(matches!(
            parse(CHILD.as_bytes(), path),
            Err(ClaudeParseError::InvalidSourcePath)
        ));
    }

    let mut later = final_record();
    later["cwd"] = json!("/later");
    later["version"] = json!({"unknown": [1, 2]});
    let earlier =
        json!({"type": {"unknown": true}, "timestamp": "2025-01-01T00:00:00Z", "cwd": "/earlier"});
    let parsed = parse(format!("{later}\n{earlier}").as_bytes(), SOURCE_PATH).unwrap();
    assert_eq!(time(parsed.metadata.started_at), "2025-01-01T00:00:00Z");
    assert_eq!(
        parsed.metadata.working_directory.unwrap(),
        Path::new("/earlier")
    );
}

#[test]
fn invalid_counters_and_overflow_reject_the_session() {
    for (field, invalid) in [
        ("input_tokens", json!(-1)),
        ("output_tokens", json!(1.5)),
        ("cache_read_input_tokens", json!("PRIVATE_COUNTER")),
        ("cache_creation_input_tokens", Value::Null),
    ] {
        let mut record = final_record();
        record["message"]["usage"][field] = invalid;
        record["message"]["stop_reason"] = Value::Null;
        let error = parse_record(&record).unwrap_err();
        assert!(matches!(
            error,
            ClaudeParseError::InvalidField {
                field: "required counter",
                ..
            }
        ));
        assert!(!format!("{error:?} {error}").contains("PRIVATE"));
    }
    let mut record = final_record();
    record["message"]["usage"]["cache_creation"] = json!({
        "ephemeral_5m_input_tokens": u64::MAX, "ephemeral_1h_input_tokens": 1,
    });
    assert!(parse_record(&record).is_err());
    record["message"]["usage"] = json!({
        "input_tokens": u64::MAX, "output_tokens": 0,
        "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0,
    });
    assert_eq!(
        parse_record(&record).unwrap().events[0].tokens.input,
        u64::MAX
    );
    let mut message_iteration = record["message"]["usage"].clone();
    message_iteration["type"] = json!("message");
    let compaction = json!({"type": "compaction", "input_tokens": 1, "output_tokens": 0,
        "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    record["message"]["usage"]["iterations"] = json!([message_iteration, compaction]);
    assert!(matches!(
        parse_record(&record),
        Err(ClaudeParseError::InvalidField {
            field: "counter overflow",
            ..
        })
    ));

    for field in [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        let mut record = final_record();
        let mut iteration = record["message"]["usage"].clone();
        iteration["type"] = json!("message");
        iteration.as_object_mut().unwrap().remove("cache_creation");
        iteration[field] = json!(iteration[field].as_u64().unwrap() + 1);
        record["message"]["usage"]["iterations"] = json!([iteration]);
        assert!(matches!(
            parse_record(&record),
            Err(ClaudeParseError::InvalidField {
                field: "non-compaction total mismatch",
                ..
            })
        ));
    }
}

#[test]
fn truncated_tails_preserve_finals_and_report_the_omission() {
    let original = final_record();
    let tail = format!("{original}\n{{\"ignored\":\"PRIVATE");
    let parsed = parse(tail.as_bytes(), SOURCE_PATH).unwrap();
    assert_eq!(parsed.events, parse_record(&original).unwrap().events);
    assert_eq!(parsed.completion, SnapshotCompletion::Partial);
    assert_eq!(parsed.notices[0].code, "truncated_tail");
    assert_eq!(parsed.notices[0].count.get(), 1);
    assert_eq!(parsed.notices[0].line.unwrap().get(), 2);
    assert!(matches!(
        parse(format!("{tail}\n").as_bytes(), SOURCE_PATH),
        Err(ClaudeParseError::MalformedLine { line: 2 })
    ));
}

#[test]
fn conversation_and_tool_payloads_do_not_contribute_usage() {
    let original = final_record();
    let mut content = original.clone();
    content["message"]["content"] = json!([{"type": "text", "text": "PRIVATE_CONVERSATION"}]);
    content["toolUseResult"] = json!({"text": "PRIVATE_TOOL", "usage": {"input_tokens": 999999}});
    let parsed = parse_record(&content).unwrap();
    assert_eq!(parsed, parse_record(&original).unwrap());
    assert!(!format!("{parsed:?}").contains("PRIVATE"));
}

#[test]
fn unsupported_finals_can_be_corrected_and_synthetic_detection_is_explicit() {
    let original = final_record();
    let mut unsupported = original.clone();
    unsupported["message"]["usage"]["iterations"] = json!([]);
    let mut placeholder = original.clone();
    placeholder["message"]["stop_reason"] = Value::Null;
    for (source, first_line) in [
        (format!("{original}\n{unsupported}\n{placeholder}"), 2),
        (format!("{unsupported}\n{unsupported}"), 1),
    ] {
        let parsed = parse(source.as_bytes(), SOURCE_PATH).unwrap();
        assert!(parsed.events.is_empty());
        assert_eq!(parsed.notices.len(), 1);
        assert_eq!(parsed.notices[0].count.get(), 1);
        assert_eq!(parsed.notices[0].line.unwrap().get(), first_line);
    }
    assert_eq!(
        parse(format!("{unsupported}\n{original}").as_bytes(), SOURCE_PATH).unwrap(),
        parse_record(&original).unwrap()
    );

    let mut zero = original.clone();
    zero["message"]["id"] = json!("arbitrary-response-id");
    zero["message"]["usage"] = json!({"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    assert_eq!(parse_record(&zero).unwrap().events.len(), 1);
    zero["message"]["model"] = json!("<synthetic>");
    assert!(parse_record(&zero).unwrap().events.is_empty());
    zero["message"]["usage"]["input_tokens"] = json!(1);
    assert_eq!(parse_record(&zero).unwrap().events.len(), 1);
}
