use super::parse_records as parse;
use crate::support::records;

use serde_json::{Value, json};
use token_tracker::domain::{
    CacheDetail, OpenAiBilling, PricingContext, RequestBreakdown, UsageEvent,
};

const RESPONSE: &str = include_str!("../../fixtures/codex/response-mirrors.jsonl");
const LEGACY: &str = include_str!("../../fixtures/codex/legacy-resume-compaction.jsonl");

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
    let baseline = parse(&fixture[..4]).unwrap();
    assert_eq!(baseline.events.len(), 1);
    let pricing = facts(&baseline.events[0]);
    assert_eq!(pricing.requests, RequestBreakdown::SingleRequest);
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
        facts_mut(&mut expected).cache_detail = detail(usage_complete);
        assert_eq!(parse(&history[..4]).unwrap().events, vec![expected.clone()]);
        assert_eq!(parse(&history).unwrap().events, vec![expected]);
    }
}

#[test]
fn detail_only_response_corrections_replace_completeness_but_keep_request_context() {
    let fixture = records(RESPONSE);
    for initially_complete in [false, true] {
        let mut history = fixture[..5].to_vec();
        cache_field(&mut history[3]["payload"]["usage"], initially_complete);
        history.insert(1, fixture[6].clone());
        let original = parse(&history).unwrap();
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
        facts_mut(&mut expected).cache_detail = detail(!initially_complete);
        assert_eq!(parse(&history).unwrap().events, vec![expected]);
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

        let parsed = parse(&history).unwrap();
        assert_eq!(parsed.events.len(), 3);
        for (event, complete) in parsed.events.iter().zip([
            first_complete,
            first_complete && second_complete,
            second_complete,
        ]) {
            let pricing = facts(event);
            assert_eq!(pricing.cache_detail, detail(complete));
            assert!(matches!(
                pricing.requests,
                RequestBreakdown::KnownRequests(_)
            ));
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
        let original = parse(&history).unwrap();
        let mut repeat = history[4].clone();
        cache_field(
            &mut repeat["payload"]["info"]["total_token_usage"],
            !initially_complete,
        );
        history.push(repeat);
        assert_eq!(parse(&history).unwrap().events, original.events);

        for index in [6, 12] {
            let mut next = fixture[index].clone();
            cache_field(&mut next["payload"]["info"]["total_token_usage"], true);
            history.push(next);
        }
        let parsed = parse(&history).unwrap();
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(
            facts(&parsed.events[0]).cache_detail,
            detail(initially_complete)
        );
    }
}

fn facts(event: &UsageEvent) -> &OpenAiBilling {
    let Some(PricingContext::OpenAi(context)) = event.pricing_context.as_ref() else {
        panic!("expected OpenAI billing");
    };
    context
}

fn facts_mut(event: &mut UsageEvent) -> &mut OpenAiBilling {
    let Some(PricingContext::OpenAi(context)) = event.pricing_context.as_mut() else {
        panic!("expected OpenAI billing");
    };
    context
}
