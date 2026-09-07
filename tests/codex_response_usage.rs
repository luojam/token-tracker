use std::fs;
use std::io::Cursor;
use std::path::Path;

use serde_json::{Value, json};
use token_tracker::adapters::codex::{CodexParseError, CodexSessionParser};
use token_tracker::application::{ParseCompletion, ParseContext, ParsedSession, SessionParser};
use token_tracker::core::{AgentId, TokenCounts, UsageKind};

const RESPONSE: &str = include_str!("fixtures/codex/response-mirrors.jsonl");
const CORRECTION: &str = include_str!("fixtures/codex/response-repeat-correction.jsonl");

fn parse(source: &str) -> Result<ParsedSession, CodexParseError> {
    CodexSessionParser::new().parse(
        &mut Cursor::new(source),
        ParseContext {
            source_path: Path::new("/not-read/rollout-response.jsonl"),
        },
    )
}

fn prefix(source: &str, lines: usize) -> String {
    source.lines().take(lines).collect::<Vec<_>>().join("\n")
}

fn response() -> Value {
    serde_json::from_str(RESPONSE.lines().nth(3).unwrap()).unwrap()
}

fn parse_response(response: &Value) -> Result<ParsedSession, CodexParseError> {
    parse(&format!("{}\n{response}", prefix(RESPONSE, 3)))
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
fn response_fixtures_match_accounting_oracles() {
    let expectations: Value =
        serde_json::from_str(include_str!("fixtures/codex/expectations.json")).unwrap();
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex");

    for (name, expected) in expectations["fixtures"].as_object().unwrap() {
        if !name.starts_with("response-") {
            continue;
        }
        let source = fs::read_to_string(fixture_root.join(name)).unwrap();
        let parsed = parse(&source).unwrap_or_else(|error| panic!("{name}: {error}"));
        let events = expected["events"].as_array().unwrap();
        assert_eq!(parsed.events.len(), events.len(), "{name}");
        for (actual, expected) in parsed.events.iter().zip(events) {
            assert_eq!(actual.identity.agent, AgentId::from("codex"), "{name}");
            assert_eq!(actual.identity.adapter_key, expected["key"], "{name}");
            assert_eq!(tokens(actual.tokens), expected["tokens"], "{name}");
            assert_eq!(actual.kind, UsageKind::Other, "{name}");
            assert_eq!(actual.recorded_cost, None, "{name}");
        }
        let total: u128 = parsed.events.iter().map(|event| event.tokens.total()).sum();
        assert_eq!(
            total,
            u128::from(expected["total_tokens"].as_u64().unwrap()),
            "{name}"
        );
        let completion = if expected["completion"] == "incomplete_final_line" {
            ParseCompletion::IncompleteFinalLine
        } else {
            ParseCompletion::Complete
        };
        assert_eq!(parsed.completion, completion, "{name}");
    }
}

#[test]
fn response_prefixes_count_usage_before_mirrors_arrive() {
    let expectations: Value =
        serde_json::from_str(include_str!("fixtures/codex/expectations.json")).unwrap();
    for (lines, expected) in expectations["prefixes"]["response-mirrors.jsonl"]
        .as_object()
        .unwrap()
    {
        let parsed = parse(&prefix(RESPONSE, lines.parse().unwrap())).unwrap();
        let actual: Vec<_> = parsed
            .events
            .iter()
            .map(|event| json!([event.identity.adapter_key, tokens(event.tokens)]))
            .collect();
        assert_eq!(json!(actual), *expected, "prefix {lines}");
    }

    let without_mirrors = RESPONSE
        .lines()
        .filter(|line| !line.contains("\"token_count\""))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(parse(&without_mirrors).is_err());
}

#[test]
fn repeated_responses_and_corrections_keep_original_identity_and_time() {
    let original = parse(&prefix(CORRECTION, 5)).unwrap();
    assert_eq!(
        parse(&prefix(CORRECTION, 7)).unwrap().events,
        original.events
    );

    let mut correction: Value = serde_json::from_str(CORRECTION.lines().last().unwrap()).unwrap();
    correction["timestamp"] = json!("2026-02-01T00:00:00Z");
    let source = format!("{}\n{correction}", prefix(CORRECTION, 7));
    let corrected = parse(&source).unwrap();
    let mut expected = original.events[0].clone();
    expected.tokens = TokenCounts {
        input: 100,
        cache_read: 50,
        cache_write: 0,
        output: 20,
    };
    assert_eq!(corrected.events, vec![expected]);

    let mut zero = correction.clone();
    zero["timestamp"] = json!("2026-01-01T00:00:01Z");
    for field in ["usage", "turn_token_usage", "thread_token_usage"] {
        for counter in zero["payload"][field].as_object_mut().unwrap().values_mut() {
            *counter = json!(0);
        }
    }
    let zero_correction = parse(&format!("{source}\n{zero}")).unwrap();
    let mut expected = original.events[0].clone();
    expected.tokens = TokenCounts::default();
    assert_eq!(zero_correction.events, vec![expected]);

    let zero = zero.to_string();
    let partial = format!("{source}\n{}", &zero[..zero.len() - 1]);
    let parsed = parse(&partial).unwrap();
    assert_eq!(parsed.completion, ParseCompletion::IncompleteFinalLine);
    assert_eq!(parsed.events, corrected.events);

    correction["payload"]["usage"]["total_tokens"] = json!(0);
    assert!(parse(&format!("{source}\n{correction}")).is_err());
}

#[test]
fn rejects_conflicting_response_identity_without_exposing_ids() {
    assert!(
        parse(include_str!(
            "fixtures/codex/reject-response-identity.jsonl"
        ))
        .is_err()
    );
    for field in ["thread_id", "turn_id", "session_id", "root_turn_id"] {
        let mut repeat = response();
        repeat["payload"][field] = json!("SECRET_CONFLICTING_ID");
        let error = parse(&format!("{}\n{repeat}", prefix(RESPONSE, 5))).unwrap_err();
        assert!(matches!(
            error,
            CodexParseError::InvalidField { line: 6, .. }
        ));
        assert!(!format!("{error:?} {error}").contains("SECRET"));
    }
    let mut unrelated = response();
    unrelated["payload"]["thread_id"] = json!("unrelated-thread");
    assert!(parse_response(&unrelated).is_err());
}

#[test]
fn validates_response_and_cumulative_counter_relationships_with_checked_math() {
    for vector in ["usage", "turn_token_usage", "thread_token_usage"] {
        for changes in [
            json!({"cached_input_tokens": 101}),
            json!({"cache_write_input_tokens": 61}),
            json!({"reasoning_output_tokens": 11}),
            json!({"total_tokens": 111}),
            json!({"input_tokens": u64::MAX, "total_tokens": 9}),
            json!({"cached_input_tokens": u64::MAX, "cache_write_input_tokens": 1}),
        ] {
            let mut changed = response();
            for (counter, value) in changes.as_object().unwrap() {
                changed["payload"][vector][counter] = value.clone();
            }
            changed["payload"]["content"] = json!("SECRET_IGNORED_CONTENT");
            let error = parse_response(&changed).unwrap_err();
            match error {
                CodexParseError::InvalidField { line, field } => {
                    assert_eq!(line, 4);
                    assert_eq!(field, format!("token_usage_record.payload.{vector}"));
                }
                _ => panic!("unexpected error: {error}"),
            }
            assert!(!format!("{error:?} {error}").contains("SECRET"));
        }
    }

    for vector in ["turn_token_usage", "thread_token_usage"] {
        for changes in [
            json!({"input_tokens": 99, "total_tokens": 109}),
            json!({"cached_input_tokens": 39}),
            json!({"cached_input_tokens": 90}),
            json!({"output_tokens": 9, "total_tokens": 109}),
            json!({"reasoning_output_tokens": 1}),
            json!({"cache_write_input_tokens": 0}),
            json!({"cache_write_input_tokens": 50}),
        ] {
            let mut changed = response();
            for field in ["usage", "turn_token_usage", "thread_token_usage"] {
                changed["payload"][field]["cache_write_input_tokens"] = json!(1);
            }
            for (counter, value) in changes.as_object().unwrap() {
                changed["payload"][vector][counter] = value.clone();
            }
            if vector == "turn_token_usage" {
                changed["payload"]["thread_token_usage"] = changed["payload"][vector].clone();
            }
            assert!(parse_response(&changed).is_err(), "{vector}: {changes}");
        }
    }

    let mut largest = response();
    for vector in ["usage", "turn_token_usage", "thread_token_usage"] {
        largest["payload"][vector] = json!({
            "input_tokens": u64::MAX - 1,
            "cached_input_tokens": u64::MAX - 2,
            "cache_write_input_tokens": 1,
            "output_tokens": 1,
            "reasoning_output_tokens": 1,
            "total_tokens": u64::MAX,
        });
    }
    let parsed = parse_response(&largest).unwrap();
    assert_eq!(
        parsed.events[0].tokens,
        TokenCounts {
            input: 0,
            cache_read: u64::MAX - 2,
            cache_write: 1,
            output: 1,
        }
    );
    assert_eq!(parsed.events[0].tokens.total(), u128::from(u64::MAX));
}

#[test]
fn only_cache_write_subdivision_is_optional() {
    for vector in ["usage", "turn_token_usage", "thread_token_usage"] {
        for counter in [
            "input_tokens",
            "cached_input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "total_tokens",
        ] {
            let mut missing = response();
            missing["payload"][vector]
                .as_object_mut()
                .unwrap()
                .remove(counter);
            assert!(parse_response(&missing).is_err(), "{vector}.{counter}");
        }
    }

    let mut missing = response();
    for vector in ["usage", "turn_token_usage", "thread_token_usage"] {
        missing["payload"][vector]
            .as_object_mut()
            .unwrap()
            .remove("cache_write_input_tokens");
    }
    let parsed = parse_response(&missing).unwrap();
    assert_eq!(tokens(parsed.events[0].tokens), json!([60, 40, 0, 10]));
    assert_eq!(parsed.events[0].tokens.total(), 110);
    assert_eq!(parsed.events[0].recorded_cost, None);
}
