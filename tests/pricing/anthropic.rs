use std::io::Cursor;
use std::path::Path;
use token_tracker::adapters::files::{ParseContext, SessionParser};

use token_tracker::adapters::claude::ClaudeSessionParser;

use token_tracker::domain::{
    AnthropicBilling, CacheWriteTokens, EstimateUnavailableReason as Reason, EstimatedCost,
    KnownRequests, ModelAttribution, PricingContext, RequestBreakdown, ServiceSpeed, ServiceTier,
    TierEvidence, Timestamp, TokenCounts, UsageEstimate, UsageEvent, UsageEventIdentity, UsageKind,
};
use token_tracker::pricing::anthropic::calculate_estimate;

fn event() -> UsageEvent {
    let tokens = TokenCounts {
        input: 10,
        output: 20,
        cache_read: 100,
        cache_write: 40,
    };
    UsageEvent {
        identity: UsageEventIdentity {
            agent: "claude".into(),
            adapter_key: "response-v1:price".into(),
        },
        timestamp: Timestamp::from_unix_milliseconds(0),
        kind: UsageKind::Assistant,
        attribution: Some(ModelAttribution {
            provider: "anthropic".into(),
            model: "claude-opus-5".into(),
        }),
        tokens,
        recorded_cost: None,
        pricing_context: Some(PricingContext::Anthropic(AnthropicBilling {
            tier: ServiceTier::Standard,
            speed: ServiceSpeed::Standard,
            tier_evidence: TierEvidence::ServedResponse,
            requests: RequestBreakdown::KnownRequests(KnownRequests::new(tokens)),
            cache_writes: Some(vec![
                CacheWriteTokens {
                    duration_seconds: 300,
                    tokens: 30,
                },
                CacheWriteTokens {
                    duration_seconds: 3600,
                    tokens: 10,
                },
            ]),
        })),
    }
}

fn facts(event: &mut UsageEvent) -> &mut AnthropicBilling {
    let Some(PricingContext::Anthropic(context)) = event.pricing_context.as_mut() else {
        panic!("expected Anthropic billing");
    };
    context
}

fn expected(cost: u128) -> Result<UsageEstimate, Reason> {
    Ok(UsageEstimate {
        cost: EstimatedCost::from_picodollars(cost),
        assumed_cache_writes_as_input: false,
    })
}

#[test]
fn fixture_costs_include_cache_durations_and_compaction_once() {
    let parsed = ClaudeSessionParser::new()
        .parse(
            &mut Cursor::new(include_bytes!("../fixtures/claude/cache-iterations.jsonl")),
            ParseContext {
                source_path: Path::new("/invented/11111111-1111-4111-8111-111111111111.jsonl"),
            },
        )
        .unwrap();
    let cases = [
        ("msg_oracle", expected(887_500_000)),
        ("msg_5m", expected(106_000_000)),
        ("msg_1h", expected(242_000_000)),
        ("msg_compaction", expected(1_161_500_000)),
        ("msg_missing_duration", Err(Reason::IncompleteCacheDetail)),
        ("msg_null_evidence", Err(Reason::UnknownTier)),
        ("msg_missing_speed", Err(Reason::UnknownSpeed)),
        ("msg_opaque", Err(Reason::UnsupportedProvider)),
        ("msg_unattributed", Err(Reason::UnknownAttribution)),
        ("msg_unknown_model", Err(Reason::UnsupportedModel)),
        ("msg_unknown_tier", Err(Reason::UnsupportedTier)),
    ];
    assert_eq!(parsed.events.len(), cases.len());
    for (id, result) in cases {
        let event = parsed
            .events
            .iter()
            .find(|event| event.identity.adapter_key == format!("response-v1:{id}"))
            .unwrap();
        assert_eq!(calculate_estimate(event), result, "{id}");
    }
}

#[test]
fn bundled_models_use_exact_flat_rates() {
    for (model, speed, oracle, long_input) in [
        (
            "claude-fable-5",
            ServiceSpeed::Standard,
            1_775_000_000,
            9_000_000_000_000,
        ),
        (
            "claude-fable-5-1",
            ServiceSpeed::Standard,
            1_700_000_000,
            9_000_000_000_000,
        ),
        (
            "claude-opus-5",
            ServiceSpeed::Standard,
            887_500_000,
            4_500_000_000_000,
        ),
        (
            "claude-opus-5",
            ServiceSpeed::Fast,
            1_775_000_000,
            9_000_000_000_000,
        ),
        (
            "claude-sonnet-5",
            ServiceSpeed::Standard,
            355_000_000,
            1_800_000_000_000,
        ),
        (
            "claude-haiku-4-5-20251001",
            ServiceSpeed::Standard,
            177_500_000,
            900_000_000_000,
        ),
    ] {
        let mut event = event();
        event.attribution.as_mut().unwrap().model = model.into();
        facts(&mut event).speed = speed.clone();
        assert_eq!(
            calculate_estimate(&event),
            expected(oracle),
            "{model}/{speed:?}"
        );
        for (input, cost) in [(0, 0), (900_000, long_input)] {
            event.tokens = TokenCounts {
                input,
                ..TokenCounts::default()
            };
            facts(&mut event).requests =
                RequestBreakdown::KnownRequests(KnownRequests::new(event.tokens));
            facts(&mut event).cache_writes = None;
            assert_eq!(
                calculate_estimate(&event),
                expected(cost),
                "{model}/{speed:?}"
            );
        }
    }
    let mut event = event();
    event.tokens = TokenCounts {
        input: u64::MAX,
        output: u64::MAX,
        cache_read: u64::MAX,
        cache_write: u64::MAX,
    };
    facts(&mut event).requests = RequestBreakdown::KnownRequests(KnownRequests::new(event.tokens));
    facts(&mut event).cache_writes = Some(vec![CacheWriteTokens {
        duration_seconds: 3600,
        tokens: u64::MAX,
    }]);
    assert_eq!(
        calculate_estimate(&event),
        expected(u128::from(u64::MAX) * 40_500_000)
    );
}

#[test]
fn only_served_tier_evidence_allows_pricing() {
    for evidence in [TierEvidence::Unknown, TierEvidence::RequestedSetting] {
        let mut event = event();
        facts(&mut event).tier_evidence = evidence;
        assert_eq!(calculate_estimate(&event), Err(Reason::UnknownTier));
    }
}

#[test]
fn unsupported_facts_report_the_most_specific_reason() {
    for (tier, speed, reason) in [
        (
            ServiceTier::Unknown,
            ServiceSpeed::Standard,
            Reason::UnknownTier,
        ),
        (
            ServiceTier::Unsupported("standard_only".into()),
            ServiceSpeed::Standard,
            Reason::UnsupportedTier,
        ),
        (
            ServiceTier::Standard,
            ServiceSpeed::Unknown,
            Reason::UnknownSpeed,
        ),
        (
            ServiceTier::Standard,
            ServiceSpeed::Unsupported("FAST".into()),
            Reason::UnsupportedSpeed,
        ),
        (
            ServiceTier::Unsupported("future".into()),
            ServiceSpeed::Unknown,
            Reason::UnsupportedTier,
        ),
    ] {
        let mut event = event();
        let context = facts(&mut event);
        context.tier = tier;
        context.speed = speed;
        assert_eq!(calculate_estimate(&event), Err(reason));
    }
    for model in [
        "claude-fable-5",
        "claude-fable-5-1",
        "claude-sonnet-5",
        "claude-haiku-4-5-20251001",
    ] {
        let mut event = event();
        event.attribution.as_mut().unwrap().model = model.into();
        facts(&mut event).speed = ServiceSpeed::Fast;
        assert_eq!(calculate_estimate(&event), Err(Reason::UnsupportedSpeed));
    }
    for model in ["claude-opus-5[1m]", "claude-haiku-4-5"] {
        let mut event = event();
        event.attribution.as_mut().unwrap().model = model.into();
        facts(&mut event).speed = ServiceSpeed::Unknown;
        assert_eq!(calculate_estimate(&event), Err(Reason::UnsupportedModel));
    }
    let mut event = event();
    event.pricing_context = None;
    assert_eq!(
        calculate_estimate(&event),
        Err(Reason::MissingPricingContext)
    );
}

#[test]
fn invalid_usage_and_missing_iteration_durations_never_produce_partial_costs() {
    let mut event = event();
    let original = event.tokens;
    event.tokens = original.checked_add(original).unwrap();
    facts(&mut event).requests =
        RequestBreakdown::KnownRequests(KnownRequests::from_vec(vec![original, original]).unwrap());
    facts(&mut event).cache_writes = None;
    assert_eq!(
        calculate_estimate(&event),
        Err(Reason::IncompleteCacheDetail)
    );

    event.tokens.input += 1;
    assert_eq!(
        calculate_estimate(&event),
        Err(Reason::InvalidUsageBreakdown)
    );
}
