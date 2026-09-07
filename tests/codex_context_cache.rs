use std::io::Cursor;
use std::path::Path;

use serde_json::{Value, json};
use token_tracker::adapters::codex::CodexSessionParser;
use token_tracker::application::{ParseContext, ParsedSession, SessionParser};
use token_tracker::core::{CacheDetail, RequestGranularity};

const RESPONSE: &str = include_str!("fixtures/codex/response-mirrors.jsonl");
const LEGACY: &str = include_str!("fixtures/codex/legacy-resume-compaction.jsonl");

fn records(source: &str) -> Vec<Value> {
    source
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn parse(records: &[Value]) -> ParsedSession {
    let source = records
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    CodexSessionParser::new()
        .parse(
            &mut Cursor::new(source),
            ParseContext {
                source_path: Path::new("/not-read/rollout-context-cache.jsonl"),
            },
        )
        .unwrap()
}

fn cache_field(vector: &mut Value, present: bool) {
    if present {
        vector["cache_write_input_tokens"] = json!(0);
    } else {
        vector
            .as_object_mut()
            .unwrap()
            .remove("cache_write_input_tokens");
    }
}

fn detail(complete: bool) -> CacheDetail {
    if complete {
        CacheDetail::Complete
    } else {
        CacheDetail::Incomplete
    }
}

#[test]
fn response_cache_detail_comes_only_from_usage_not_totals_or_mirrors() {
    let fixture = records(RESPONSE);
    let baseline = parse(&fixture[..4]);
    assert_eq!(baseline.events.len(), 1);
    let pricing = baseline.events[0].pricing_context.as_ref().unwrap();
    assert_eq!(
        pricing.request_granularity,
        RequestGranularity::ExactSingleRequest
    );
    for usage_complete in [false, true] {
        let mut history = fixture[..5].to_vec();
        cache_field(&mut history[3]["payload"]["usage"], usage_complete);
        for vector in ["turn_token_usage", "thread_token_usage"] {
            cache_field(&mut history[3]["payload"][vector], !usage_complete);
        }
        for vector in ["total_token_usage", "last_token_usage"] {
            cache_field(&mut history[4]["payload"]["info"][vector], !usage_complete);
        }
        let mut expected = baseline.events[0].clone();
        expected.pricing_context.as_mut().unwrap().cache_detail = detail(usage_complete);
        assert_eq!(parse(&history[..4]).events, vec![expected.clone()]);
        assert_eq!(parse(&history).events, vec![expected]);
    }
}

#[test]
fn detail_only_response_corrections_replace_completeness_but_keep_request_context() {
    let fixture = records(RESPONSE);
    for initially_complete in [false, true] {
        let mut history = fixture[..5].to_vec();
        cache_field(&mut history[3]["payload"]["usage"], initially_complete);
        history.insert(1, fixture[6].clone());
        let original = parse(&history);
        assert_eq!(original.events.len(), 1);

        history.push(fixture[5].clone());
        let mut settings = fixture[6].clone();
        settings["payload"]["thread_settings"]["service_tier"] = json!("default");
        history.push(settings);
        history.extend_from_slice(&fixture[7..9]);
        history.last_mut().unwrap()["payload"]["model"] = json!("gpt-5.6");
        let mut correction = fixture[3].clone();
        correction["timestamp"] = json!("2026-02-01T00:00:00Z");
        cache_field(&mut correction["payload"]["usage"], !initially_complete);
        history.push(correction);

        let mut expected = original.events[0].clone();
        expected.pricing_context.as_mut().unwrap().cache_detail = detail(!initially_complete);
        assert_eq!(parse(&history).events, vec![expected]);
    }
}

#[test]
fn legacy_cache_detail_requires_both_delta_endpoints_and_recovers_on_later_turns() {
    let fixture = records(LEGACY);
    for (first_complete, second_complete) in [(false, true), (true, false), (true, true)] {
        let mut history = fixture[..5].to_vec();
        cache_field(
            &mut history[4]["payload"]["info"]["total_token_usage"],
            first_complete,
        );
        history.push(fixture[7].clone());
        history.extend_from_slice(&fixture[9..11]);
        let mut second = fixture[6].clone();
        cache_field(
            &mut second["payload"]["info"]["total_token_usage"],
            second_complete,
        );
        cache_field(&mut second["payload"]["info"]["last_token_usage"], true);
        history.push(second);
        history.push(fixture[13].clone());
        for record in &fixture[9..11] {
            let mut record = record.clone();
            record["payload"]["turn_id"] = json!("turn-legacy-c");
            history.push(record);
        }
        let mut third = fixture[12].clone();
        cache_field(&mut third["payload"]["info"]["total_token_usage"], true);
        history.push(third);

        let parsed = parse(&history);
        assert_eq!(parsed.events.len(), 3);
        for (event, complete) in parsed.events.iter().zip([
            first_complete,
            first_complete && second_complete,
            second_complete,
        ]) {
            let pricing = event.pricing_context.as_ref().unwrap();
            assert_eq!(pricing.cache_detail, detail(complete));
            assert_eq!(
                pricing.request_granularity,
                RequestGranularity::AggregateOrUnknown
            );
        }
    }
}

#[test]
fn legacy_uncertainty_is_sticky_within_an_aggregate_and_noops_preserve_context() {
    let fixture = records(LEGACY);
    for initially_complete in [false, true] {
        let mut history = fixture[..5].to_vec();
        cache_field(
            &mut history[4]["payload"]["info"]["total_token_usage"],
            initially_complete,
        );
        let original = parse(&history);
        let mut repeat = history[4].clone();
        cache_field(
            &mut repeat["payload"]["info"]["total_token_usage"],
            !initially_complete,
        );
        history.push(repeat);
        assert_eq!(parse(&history).events, original.events);

        for index in [6, 12] {
            let mut next = fixture[index].clone();
            cache_field(&mut next["payload"]["info"]["total_token_usage"], true);
            history.push(next);
        }
        let parsed = parse(&history);
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(
            parsed.events[0]
                .pricing_context
                .as_ref()
                .unwrap()
                .cache_detail,
            detail(initially_complete)
        );
    }
}
