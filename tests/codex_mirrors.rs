use std::io::Cursor;
use std::path::Path;

use serde_json::{Value, json};
use token_tracker::adapters::codex::{CodexParseError, CodexSessionParser};
use token_tracker::application::{ParseContext, ParsedSession, SessionParser};

const RESPONSE: &str = include_str!("fixtures/codex/response-mirrors.jsonl");
const UPGRADE: &str = include_str!("fixtures/codex/upgrade-response-first.jsonl");

fn records(source: &str) -> Vec<Value> {
    source
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn parse(records: &[Value]) -> Result<ParsedSession, CodexParseError> {
    let source = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    CodexSessionParser::new().parse(
        &mut Cursor::new(source),
        ParseContext {
            source_path: Path::new("/not-read/rollout-mirrors.jsonl"),
        },
    )
}

fn multiply(vector: &mut Value, factor: u64) {
    for counter in vector.as_object_mut().unwrap().values_mut() {
        *counter = json!(counter.as_u64().unwrap() * factor);
    }
}

#[test]
fn validates_raw_mirror_offset_last_usage_and_unmatched_progression() {
    for source in [RESPONSE, UPGRADE] {
        let original = records(source);
        let mirror = if source == RESPONSE { 4 } else { 12 };
        for field in ["total_token_usage", "last_token_usage"] {
            for counter in [
                "input_tokens",
                "cached_input_tokens",
                "cache_write_input_tokens",
                "output_tokens",
                "reasoning_output_tokens",
                "total_tokens",
            ] {
                let mut changed = original.clone();
                let vector = &mut changed[mirror]["payload"]["info"][field];
                vector[counter] = json!(vector[counter].as_u64().unwrap() + 1);
                let error = parse(&changed).unwrap_err();
                assert!(
                    matches!(error, CodexParseError::InvalidField { line, .. } if line == mirror + 1),
                    "{field}.{counter}: {error}"
                );
            }
        }
        let mut unmatched = original[..=mirror].to_vec();
        let mut notification = unmatched[mirror].clone();
        multiply(&mut notification["payload"]["info"]["total_token_usage"], 2);
        unmatched.push(notification);
        assert!(parse(&unmatched).is_err());
    }
}

#[test]
fn response_accumulators_must_match_original_progression_and_turn_boundaries() {
    let original = records(RESPONSE);
    for index in [3, 9] {
        for field in ["turn_token_usage", "thread_token_usage"] {
            let mut changed = original.clone();
            multiply(&mut changed[index]["payload"][field], 2);
            assert!(parse(&changed[..=index]).is_err(), "{index}: {field}");
        }
    }
    let mut wrong_turn = original.clone();
    wrong_turn[9]["payload"]["turn_id"] = json!("turn-main");
    assert!(parse(&wrong_turn).is_err());

    let mut wrong_thread = records(UPGRADE);
    let mut ancestor = wrong_thread[0].clone();
    ancestor["payload"]["id"] = json!("thread-ancestor");
    wrong_thread[0]["payload"]["forked_from_id"] = json!("thread-ancestor");
    wrong_thread[8]["payload"]["forked_from_id"] = json!("thread-ancestor");
    wrong_thread[11]["payload"]["thread_id"] = json!("thread-ancestor");
    wrong_thread.insert(1, ancestor);
    assert!(parse(&wrong_thread).is_err());

    let mut checkpoint = original.clone();
    checkpoint.insert(
        1,
        json!({"timestamp":"2026-01-01T00:00:00Z","type":"compacted","payload":{}}),
    );
    assert!(parse(&checkpoint).is_err());
}

#[test]
fn pending_response_allows_identical_repeats_but_not_corrections_or_unproven_boundaries() {
    assert!(
        parse(&records(include_str!(
            "fixtures/codex/reject-ambiguous-mirrors.jsonl"
        )))
        .is_err()
    );
    let original = records(RESPONSE);
    let mut pending = original[..4].to_vec();
    pending.push(original[3].clone());
    assert_eq!(
        parse(&pending).unwrap().events,
        parse(&original[..4]).unwrap().events
    );
    pending.push(original[4].clone());
    pending.extend_from_slice(&original[5..]);
    assert_eq!(
        parse(&pending).unwrap().events,
        parse(&original).unwrap().events
    );

    for field in ["usage", "turn_token_usage", "thread_token_usage"] {
        let mut changed = original[..4].to_vec();
        let mut correction = original[3].clone();
        // A smaller usage remains within cumulative bounds but is not a proven correction order.
        multiply(
            &mut correction["payload"][field],
            if field == "usage" { 0 } else { 2 },
        );
        changed.push(correction);
        assert!(parse(&changed).is_err());
    }
    for next in [&original[5], &original[7], &original[9]] {
        let mut changed = original[..4].to_vec();
        changed.push(next.clone());
        assert!(parse(&changed).is_err());
    }
}

#[test]
fn confirmed_corrections_do_not_change_historical_mirror_arithmetic() {
    let original = records(RESPONSE);
    let correction = records(include_str!(
        "fixtures/codex/response-repeat-correction.jsonl"
    ))
    .pop()
    .unwrap();
    let mut changed = original[..5].to_vec();
    changed.push(correction.clone());
    changed.extend_from_slice(&original[5..]);
    let parsed = parse(&changed).unwrap();
    assert_eq!(parsed.events.len(), 2);
    assert_eq!(
        parsed
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u128>(),
        310
    );

    let mut invented_mirror = original[4].clone();
    invented_mirror["payload"]["info"]["total_token_usage"] =
        correction["payload"]["thread_token_usage"].clone();
    invented_mirror["payload"]["info"]["last_token_usage"] = correction["payload"]["usage"].clone();
    changed.insert(6, invented_mirror);
    assert!(parse(&changed).is_err());
}

#[test]
fn repeated_snapshots_compaction_and_zero_mirrors_do_not_add_events() {
    let original = records(RESPONSE);
    let mut changed = original[..5].to_vec();
    changed.push(json!({"timestamp":"2026-01-01T00:00:00Z","type":"compacted","payload":{}}));
    let mut recomputed = original[4].clone();
    multiply(&mut recomputed["payload"]["info"]["last_token_usage"], 0);
    recomputed["payload"]["info"]["last_token_usage"]["total_tokens"] = json!(50);
    changed.push(recomputed.clone());
    changed.push(json!({"timestamp":"2026-01-01T00:00:00Z","type":"event_msg","payload":{"type":"token_count","info":null}}));
    changed.extend_from_slice(&original[5..10]);
    changed.push(recomputed);
    changed.extend_from_slice(&original[10..]);
    assert_eq!(
        parse(&changed).unwrap().events,
        parse(&original).unwrap().events
    );

    let mut zero = original.clone();
    for field in ["usage", "turn_token_usage", "thread_token_usage"] {
        multiply(&mut zero[3]["payload"][field], 0);
    }
    for field in ["total_token_usage", "last_token_usage"] {
        multiply(&mut zero[4]["payload"]["info"][field], 0);
    }
    zero[9]["payload"]["thread_token_usage"] = zero[9]["payload"]["usage"].clone();
    zero[10]["payload"]["info"]["total_token_usage"] = zero[9]["payload"]["usage"].clone();
    let parsed = parse(&zero).unwrap();
    assert_eq!(parsed.events.len(), 2);
    assert_eq!(parsed.events[0].tokens.total(), 0);
    assert_eq!(parsed.events[1].tokens.total(), 140);
}

#[test]
fn adding_response_usage_to_a_legacy_offset_is_checked_for_overflow() {
    let mut source = records(UPGRADE);
    let largest = json!({"input_tokens":u64::MAX,"cached_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":u64::MAX});
    for index in [4, 5, 6] {
        source[index]["payload"]["info"]["total_token_usage"] = largest.clone();
        source[index]["payload"]["info"]["last_token_usage"] = largest.clone();
    }
    assert!(parse(&source[..11]).is_ok());
    assert!(parse(&source[..12]).is_err());
}
