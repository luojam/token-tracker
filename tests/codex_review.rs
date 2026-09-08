use std::io::Cursor;
use std::path::Path;

use serde_json::{Value, json};
use token_tracker::adapters::codex::{CodexParseError, CodexSessionParser};
use token_tracker::application::{ParseContext, ParsedSession, SessionParser};
use token_tracker::core::ServiceTier;

fn records() -> Vec<Value> {
    include_str!("fixtures/codex/response-mirrors.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn parse(lines: &[Value]) -> Result<ParsedSession, CodexParseError> {
    let source = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    CodexSessionParser::new().parse(
        &mut Cursor::new(source),
        ParseContext {
            source_path: Path::new("/not-read/rollout-review.jsonl"),
        },
    )
}

fn entry(kind: &str, payload: Value) -> Value {
    json!({"timestamp": "2026-01-01T00:00:07Z", "type": kind, "payload": payload})
}

fn boundary(kind: &str, turn: &str) -> Value {
    entry("event_msg", json!({"type": kind, "turn_id": turn}))
}

fn review(item_markers: bool) -> Vec<Value> {
    let markers = if item_markers {
        ["EnteredReviewMode", "ExitedReviewMode"]
    } else {
        ["entered_review_mode", "exited_review_mode"]
    };
    let [enter, exit] = markers.map(|kind| {
        if item_markers {
            entry(
                "event_msg",
                json!({
                    "type": "item_completed", "thread_id": "thread-main", "turn_id": "review",
                    "item": {"type": kind, "id": kind, "review": "synthetic review"}
                }),
            )
        } else {
            boundary(kind, "review")
        }
    });
    vec![
        enter,
        boundary("task_started", "child"),
        exit,
        boundary("task_complete", "review"),
    ]
}

#[test]
fn review_prefixes_and_terminals_preserve_surrounding_usage() {
    for item_markers in [false, true] {
        for legacy_usage in [false, true] {
            let mut normal = records();
            if legacy_usage {
                normal.retain(|line| line["type"] != "token_usage_record");
            }
            let split = if legacy_usage { 5 } else { 6 };
            let before = parse(&normal[..split]).unwrap();
            let expected = parse(&normal).unwrap();
            assert_eq!(expected.events.len(), 2);
            for child in [false, true] {
                for terminal in ["task_complete", "turn_aborted"] {
                    let mut envelope = review(item_markers);
                    envelope[3] = boundary(terminal, "review");
                    if !child {
                        envelope.remove(1);
                    }
                    let mut lines = normal[..split].to_vec();
                    for event in envelope {
                        lines.push(event);
                        assert_eq!(parse(&lines).unwrap(), before);
                    }
                    lines.extend_from_slice(&normal[split..]);
                    assert_eq!(parse(&lines).unwrap(), expected);
                }
            }
        }
    }
}

#[test]
fn review_rejects_mismatched_lifecycle_and_unexpected_accounting() {
    let normal = records();
    for item_markers in [false, true] {
        let envelope = review(item_markers);
        let mut wrong_exit = envelope[2].clone();
        wrong_exit["payload"]["turn_id"] = json!("child");
        let mut response = normal[9].clone();
        response["payload"]["turn_id"] = json!("child");
        let mut invalid_owner = envelope[0].clone();
        invalid_owner["payload"][if item_markers { "thread_id" } else { "turn_id" }] = Value::Null;
        for (prefix, invalid) in [
            (0, envelope[2].clone()),
            (0, invalid_owner),
            (2, envelope[0].clone()),
            (2, wrong_exit),
            (2, boundary("task_complete", "review")),
            (3, boundary("task_complete", "child")),
            (4, envelope[0].clone()),
            (
                2,
                entry(
                    "turn_context",
                    json!({"turn_id": "child", "model": "review-model"}),
                ),
            ),
            (2, response),
            (2, normal[10].clone()),
        ] {
            let mut lines = normal[..6].to_vec();
            lines.extend_from_slice(&envelope[..prefix]);
            lines.push(invalid);
            let result = parse(&lines);
            assert!(
                matches!(result, Err(CodexParseError::InvalidField { line, .. }) if line == lines.len()),
                "item_markers={item_markers}, prefix={prefix}: {result:?}"
            );
        }
    }
}

#[test]
fn review_settings_require_parent_ownership_and_preserve_prior_pricing() {
    let normal = records();
    let mut before = normal[..6].to_vec();
    let mut initial = normal[6].clone();
    initial["payload"]["thread_settings"]["service_tier"] = json!("default");
    before.insert(1, initial);
    let prior = parse(&before).unwrap().events.remove(0);
    for (owner, expected) in [
        (json!("thread-main"), ServiceTier::Fast),
        (json!("thread-child"), ServiceTier::Standard),
        (Value::Null, ServiceTier::Unknown),
    ] {
        let mut settings = normal[6].clone();
        settings["payload"]["thread_id"] = owner;
        let mut envelope = review(true);
        envelope.insert(2, settings);
        let mut lines = before.clone();
        lines.extend(envelope);
        lines.extend_from_slice(&normal[7..]);
        let parsed = parse(&lines).unwrap();
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.events[0], prior);
        assert_eq!(parsed.events[1].attribution, prior.attribution);
        assert_eq!(
            parsed.events[1].pricing_context.as_ref().unwrap().tier,
            expected
        );
    }
}
