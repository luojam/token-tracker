use std::io::Cursor;
use std::path::Path;

use serde_json::{Value, json};
use token_tracker::adapters::codex::{CodexParseError, CodexSessionParser};
use token_tracker::application::{ParseContext, ParsedSession, SessionParser};
use token_tracker::core::{
    ModelAttribution, RawServiceTier, ServiceTier, TierEvidence, UsageEvent,
};

const RESPONSE: &str = include_str!("fixtures/codex/response-mirrors.jsonl");

fn parse(lines: &[Value]) -> Result<ParsedSession, CodexParseError> {
    let text = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    CodexSessionParser::new().parse(
        &mut Cursor::new(text),
        ParseContext {
            source_path: Path::new("/not-read/rollout-context.jsonl"),
        },
    )
}

fn fixture(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn entry(kind: &str, payload: Value) -> Value {
    json!({"timestamp": "2026-01-01T00:00:00Z", "type": kind, "payload": payload})
}

fn boundary(kind: &str, turn: &str) -> Value {
    entry("event_msg", json!({"type": kind, "turn_id": turn}))
}

fn context(turn: &str, model: Value) -> Value {
    entry("turn_context", json!({"turn_id": turn, "model": model}))
}

fn settings(thread: Option<&str>, tier: Option<Value>) -> Value {
    let mut snapshot = json!({"model": "settings-model-is-not-turn-evidence", "model_provider_id": "not-the-provider"});
    if let Some(tier) = tier {
        snapshot["service_tier"] = tier;
    }
    let mut payload = json!({"type": "thread_settings_applied", "thread_settings": snapshot});
    if let Some(thread) = thread {
        payload["thread_id"] = json!(thread);
    }
    entry("event_msg", payload)
}

fn usage(n: u64) -> Value {
    json!({"input_tokens": 100*n, "cached_input_tokens": 40*n, "cache_write_input_tokens": 0,
        "output_tokens": 10*n, "reasoning_output_tokens": 2*n, "total_tokens": 110*n})
}

fn response(id: &str, turn: &str, turn_count: u64, total: u64) -> Value {
    entry(
        "token_usage_record",
        json!({"response_id": id, "thread_id": "thread-main", "turn_id": turn,
        "usage": usage(1), "turn_token_usage": usage(turn_count), "thread_token_usage": usage(total)}),
    )
}

fn mirror(total: u64) -> Value {
    entry(
        "event_msg",
        json!({"type": "token_count", "info": {
        "total_token_usage": usage(total), "last_token_usage": usage(1)}}),
    )
}

fn assert_tier(event: &UsageEvent, tier: ServiceTier, raw: RawServiceTier, evidence: TierEvidence) {
    let context = event.pricing_context.as_ref().unwrap();
    assert_eq!(context.tier, tier);
    assert_eq!(context.raw_tier, raw);
    assert_eq!(context.tier_evidence, evidence);
}

#[test]
fn new_turn_uses_its_model_and_clears_omitted_tier() {
    let mut lines = fixture(RESPONSE);
    lines[6] = settings(None, None);
    lines[8]["payload"]["model"] = json!("new-model");
    lines.insert(1, settings(None, Some(json!("priority"))));
    let parsed = parse(&lines).unwrap();
    assert_tier(
        &parsed.events[0],
        ServiceTier::Fast,
        RawServiceTier::Value("priority".into()),
        TierEvidence::RequestedSetting,
    );
    assert_tier(
        &parsed.events[1],
        ServiceTier::Unknown,
        RawServiceTier::Missing,
        TierEvidence::RequestedSetting,
    );
    assert_eq!(
        parsed.events[1].attribution,
        Some(ModelAttribution {
            provider: "openai".into(),
            model: "new-model".into(),
        })
    );
}

#[test]
fn late_and_in_flight_settings_do_not_reprice_completed_responses() {
    let mut lines = fixture(RESPONSE)[..4].to_vec();
    lines.insert(3, settings(None, Some(json!("priority"))));
    assert_tier(
        &parse(&lines).unwrap().events[0],
        ServiceTier::Unknown,
        RawServiceTier::Missing,
        TierEvidence::Unknown,
    );

    let mut lines = vec![
        fixture(RESPONSE)[0].clone(),
        settings(None, Some(json!("default"))),
        boundary("task_started", "a"),
        context("a", json!("model-a")),
        response("a", "a", 1, 1),
        mirror(1),
    ];
    let first = parse(&lines).unwrap().events.remove(0);
    lines.extend([
        settings(Some("thread-main"), Some(json!("priority"))),
        response("b", "a", 2, 2),
        mirror(2),
        boundary("task_complete", "a"),
        boundary("task_started", "c"),
        context("c", json!("model-c")),
        response("c", "c", 1, 3),
    ]);
    let parsed = parse(&lines).unwrap();
    assert_eq!(parsed.events[0], first);
    assert_tier(
        &parsed.events[1],
        ServiceTier::Unknown,
        RawServiceTier::Value("default".into()),
        TierEvidence::Unknown,
    );
    assert_tier(
        &parsed.events[2],
        ServiceTier::Fast,
        RawServiceTier::Value("priority".into()),
        TierEvidence::RequestedSetting,
    );
}

#[test]
fn missing_provider_is_not_inferred_from_model() {
    let mut lines = fixture(RESPONSE)[..4].to_vec();
    lines[0]["payload"]
        .as_object_mut()
        .unwrap()
        .remove("model_provider");
    let parsed = parse(&lines).unwrap();
    assert_eq!(parsed.events[0].attribution, None);
    assert_eq!(parsed.events[0].tokens.total(), 110);
}

#[test]
fn legacy_aggregates_merge_only_context_of_contributing_deltas() {
    let mut lines = fixture(include_str!("fixtures/codex/legacy-fresh.jsonl"));
    lines.insert(1, settings(None, Some(json!("default"))));
    let original = parse(&lines[..6]).unwrap().events.remove(0);
    assert_eq!(
        original.pricing_context.as_ref().unwrap().tier,
        ServiceTier::Standard
    );
    lines.insert(6, settings(None, Some(json!("priority"))));
    lines.insert(7, context("turn-legacy-a", json!("changed-model")));
    // Settings, context, and the unchanged counter repeat are not new usage.
    assert_eq!(parse(&lines[..9]).unwrap().events[0], original);
    let aggregate = parse(&lines).unwrap().events.remove(0);
    assert_eq!(aggregate.tokens.total(), 250);
    assert_eq!(aggregate.attribution, None);
    assert_tier(
        &aggregate,
        ServiceTier::Unknown,
        RawServiceTier::Value("default".into()),
        TierEvidence::Unknown,
    );
}

#[test]
fn child_overrides_and_foreign_settings_never_share_parent_defaults() {
    let mut lines = fixture(include_str!("fixtures/codex/response-subagent.jsonl"));
    lines[0]["payload"]["model_provider"] = json!("child-provider");
    lines.insert(1, settings(Some("thread-main"), Some(json!("priority"))));
    let parsed = parse(&lines).unwrap();
    assert_tier(
        &parsed.events[0],
        ServiceTier::Unknown,
        RawServiceTier::Missing,
        TierEvidence::Unknown,
    );
    assert_eq!(
        parsed.events[0].attribution.as_ref().unwrap().provider,
        "child-provider"
    );
    lines.insert(2, settings(Some("thread-child"), Some(json!("default"))));
    // A foreign mid-turn event must not invalidate the child's own bound tier.
    lines.insert(5, settings(Some("thread-main"), Some(json!("priority"))));
    let parsed = parse(&lines).unwrap();
    assert_tier(
        &parsed.events[0],
        ServiceTier::Standard,
        RawServiceTier::Value("default".into()),
        TierEvidence::RequestedSetting,
    );
}

#[test]
fn explicit_fork_response_owner_resolves_model_but_not_tier() {
    for (provider, conflicting_model) in [
        (Some("child-provider"), false),
        (None, false),
        (Some("child-provider"), true),
    ] {
        let mut lines =
            fixture(include_str!("fixtures/codex/legacy-partial-fork.jsonl"))[..9].to_vec();
        lines[0]["payload"]["model_provider"] = json!(provider);
        lines[1]["payload"]["model_provider"] = json!("parent-provider");
        lines.insert(2, settings(Some("thread-parent"), Some(json!("priority"))));
        lines.insert(3, settings(Some("thread-fork"), Some(json!("default"))));
        if conflicting_model {
            lines.push(context("turn-fork", json!("conflicting-model")));
        }
        let mut owned_response = response("fork-response", "turn-fork", 1, 1);
        owned_response["payload"]["thread_id"] = json!("thread-fork");
        lines.push(owned_response);

        let parsed = parse(&lines).unwrap();
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.events[0].attribution, None);
        let event = &parsed.events[1];
        assert_eq!(event.tokens.total(), 110);
        assert_eq!(
            event.attribution,
            provider
                .filter(|_| !conflicting_model)
                .map(|provider| ModelAttribution {
                    provider: provider.into(),
                    model: "gpt-6-astra".into(),
                })
        );
        assert_tier(
            event,
            ServiceTier::Unknown,
            RawServiceTier::Missing,
            TierEvidence::Unknown,
        );
    }
}

#[test]
fn fork_turns_without_an_owner_boundary_remain_unknown() {
    for (source, child_owner_known) in [
        (include_str!("fixtures/codex/legacy-fork.jsonl"), true),
        (
            include_str!("fixtures/codex/legacy-partial-fork.jsonl"),
            false,
        ),
    ] {
        let mut lines = fixture(source);
        lines[0]["payload"]["model_provider"] = json!("child-provider");
        lines[1]["payload"]["model_provider"] = json!("parent-provider");
        lines.insert(2, settings(Some("thread-parent"), Some(json!("priority"))));
        lines.insert(3, settings(None, Some(json!("priority"))));
        for line in &mut lines[4..] {
            if line["type"] == "session_meta" {
                line["payload"]["model_provider"] = json!("child-provider");
            }
        }
        let parsed = parse(&lines).unwrap();
        assert_eq!(parsed.events.len(), 2);
        for event in &parsed.events {
            assert_tier(
                event,
                ServiceTier::Unknown,
                RawServiceTier::Missing,
                TierEvidence::Unknown,
            );
            let known = child_owner_known && event.identity.adapter_key.ends_with("turn-fork");
            assert_eq!(
                event
                    .attribution
                    .as_ref()
                    .map(|value| value.provider.as_str()),
                known.then_some("child-provider")
            );
        }
    }
}
