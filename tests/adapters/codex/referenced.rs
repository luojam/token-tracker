use super::{parse_records as parse, tokens};
use crate::support::records;
use serde_json::json;
use token_tracker::domain::{ParentSession, PricingContext, RequestBreakdown, ServiceTier};

const FORK: &str = include_str!("../../fixtures/codex/referenced-fork.jsonl");

#[test]
fn referenced_history_counts_local_requests_with_child_pricing() {
    let source = records(FORK);
    let parsed = parse(&source).unwrap();
    assert_eq!(
        parsed.metadata.parent_session,
        Some(ParentSession::SessionId("thread-parent".into()))
    );
    assert_eq!(parsed.events.len(), 2);
    assert_eq!(tokens(parsed.events[0].tokens), json!([60, 40, 0, 10]));
    assert_eq!(tokens(parsed.events[1].tokens), json!([60, 60, 0, 20]));
    for event in &parsed.events {
        let attribution = event.attribution.as_ref().unwrap();
        assert_eq!(attribution.provider, "openai");
        assert_eq!(attribution.model, "gpt-6-astra");
        let Some(PricingContext::OpenAi(context)) = &event.pricing_context else {
            panic!("missing pricing context");
        };
        assert_eq!(context.tier, ServiceTier::Fast);
        assert_eq!(context.requests, RequestBreakdown::SingleRequest);
    }
    assert!(parse(&source[..5]).unwrap().events.is_empty());
    assert_eq!(parse(&source[..6]).unwrap().events, parsed.events[..1]);

    let mut repeated = source.clone();
    repeated.insert(1, source[0].clone());
    assert_eq!(parse(&repeated).unwrap().events, parsed.events);
}

#[test]
fn interrupted_reference_closes_only_the_initial_inherited_turn() {
    let source = records(FORK);
    let abort = json!({"timestamp":"2026-01-01T00:00:00Z","type":"event_msg",
        "payload":{"type":"turn_aborted","turn_id":"inherited-turn","reason":"interrupted"}});
    let mut interrupted = source.clone();
    interrupted.insert(2, abort.clone());
    assert!(parse(&interrupted[..3]).unwrap().events.is_empty());
    assert_eq!(
        parse(&interrupted).unwrap().events,
        parse(&source).unwrap().events
    );

    interrupted.insert(3, abort.clone());
    assert!(parse(&interrupted).is_err());
    let mut local = source.clone();
    local.insert(3, abort);
    assert!(parse(&local).is_err());
}

#[test]
fn referenced_offsets_do_not_allow_later_counter_drift() {
    let source = records(FORK);
    for (index, vector) in [(11, "thread_token_usage"), (12, "total_token_usage")] {
        let mut changed = source.clone();
        let payload = &mut changed[index]["payload"];
        let counts = if index == 12 {
            &mut payload["info"][vector]
        } else {
            &mut payload[vector]
        };
        counts["cached_input_tokens"] = json!(counts["cached_input_tokens"].as_u64().unwrap() + 1);
        assert!(parse(&changed).is_err());
    }

    let mut wrong_mirror = source.clone();
    wrong_mirror[6]["payload"]["info"]["last_token_usage"]["cached_input_tokens"] = json!(41);
    assert!(parse(&wrong_mirror).is_err());

    let mut missing_reference = source;
    missing_reference[0]["payload"]
        .as_object_mut()
        .unwrap()
        .remove("history_base");
    assert!(parse(&missing_reference).is_err());
}
