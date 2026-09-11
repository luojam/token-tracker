use std::io::Cursor;
use std::path::Path;

use token_tracker::adapters::claude::ClaudeSessionParser;
use token_tracker::application::{ParseContext, SessionParser};
use token_tracker::domain::{
    CacheDetail, CacheWriteTokens, EstimateUnavailableReason as Reason, EstimatedCost,
    ModelAttribution, PricingContext, RequestGranularity, ServiceTier, TierEvidence, Timestamp,
    TokenCounts, UsageEstimate, UsageEvent, UsageEventIdentity, UsageKind,
};
use token_tracker::pricing::anthropic::calculate_estimate;

fn served(value: &str) -> ServiceTier {
    match value {
        "standard" => ServiceTier::Standard,
        "fast" => ServiceTier::Fast,
        _ => ServiceTier::Unsupported(value.into()),
    }
}

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
        pricing_context: Some(PricingContext {
            provider: "anthropic".into(),
            tier: ServiceTier::Standard,
            speed: ServiceTier::Standard,
            tier_evidence: TierEvidence::ServedResponse,
            request_granularity: RequestGranularity::ExactSingleRequest,
            cache_detail: CacheDetail::Complete,
            request_usage: Some(vec![tokens]),
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
        }),
    }
}

fn facts(event: &mut UsageEvent) -> &mut PricingContext {
    event.pricing_context.as_mut().unwrap()
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
            &mut Cursor::new(include_bytes!("fixtures/claude/cache-iterations.jsonl")),
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
fn fable_transcripts_price_cache_and_compaction() {
    for (model, cost) in [
        ("claude-fable-5", 2_323_000_000),
        ("claude-fable-5-1", 2_242_000_000),
    ] {
        let source =
            include_str!("fixtures/claude/cache-iterations.jsonl").replace("claude-opus-5", model);
        let parsed = ClaudeSessionParser::new()
            .parse(
                &mut Cursor::new(source),
                ParseContext {
                    source_path: Path::new("/invented/11111111-1111-4111-8111-111111111111.jsonl"),
                },
            )
            .unwrap();
        let event = parsed
            .events
            .iter()
            .find(|event| event.identity.adapter_key == "response-v1:msg_compaction")
            .unwrap();
        assert_eq!(
            event.attribution,
            Some(ModelAttribution {
                provider: "anthropic".into(),
                model: model.into(),
            })
        );
        assert_eq!(calculate_estimate(event), expected(cost), "{model}");
    }
}

#[test]
fn bundled_models_use_exact_flat_rates() {
    for (model, speed, oracle, long_input) in [
        (
            "claude-fable-5",
            "standard",
            1_775_000_000,
            9_000_000_000_000,
        ),
        (
            "claude-fable-5-1",
            "standard",
            1_700_000_000,
            9_000_000_000_000,
        ),
        ("claude-opus-5", "standard", 887_500_000, 4_500_000_000_000),
        ("claude-opus-5", "fast", 1_775_000_000, 9_000_000_000_000),
        (
            "claude-sonnet-5",
            "standard",
            355_000_000,
            1_800_000_000_000,
        ),
        (
            "claude-haiku-4-5-20251001",
            "standard",
            177_500_000,
            900_000_000_000,
        ),
    ] {
        let mut event = event();
        event.attribution.as_mut().unwrap().model = model.into();
        facts(&mut event).speed = served(speed);
        assert_eq!(
            calculate_estimate(&event),
            expected(oracle),
            "{model}/{speed}"
        );
        for (input, cost) in [(0, 0), (900_000, long_input)] {
            event.tokens = TokenCounts {
                input,
                ..TokenCounts::default()
            };
            facts(&mut event).request_usage = Some(vec![event.tokens]);
            facts(&mut event).cache_writes = None;
            assert_eq!(
                calculate_estimate(&event),
                expected(cost),
                "{model}/{speed}"
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
    facts(&mut event).request_usage = Some(vec![event.tokens]);
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
fn served_evidence_is_required_and_failures_have_stable_precedence() {
    for (tier, speed, reason) in [
        (
            ServiceTier::Unknown,
            served("standard"),
            Reason::UnknownTier,
        ),
        (
            served("standard_only"),
            served("standard"),
            Reason::UnsupportedTier,
        ),
        (
            served("standard"),
            ServiceTier::Unknown,
            Reason::UnknownSpeed,
        ),
        (
            served("standard"),
            ServiceTier::Unknown,
            Reason::UnknownSpeed,
        ),
        (served("standard"), served("FAST"), Reason::UnsupportedSpeed),
        (
            served("future"),
            ServiceTier::Unknown,
            Reason::UnsupportedTier,
        ),
    ] {
        let mut event = event();
        let context = event.pricing_context.as_mut().unwrap();
        context.tier = ServiceTier::Standard;
        context.tier_evidence = TierEvidence::ServedResponse;
        facts(&mut event).tier = tier;
        facts(&mut event).speed = speed;
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
        facts(&mut event).speed = served("fast");
        assert_eq!(calculate_estimate(&event), Err(Reason::UnsupportedSpeed));
    }
    for model in ["claude-opus-5[1m]", "claude-haiku-4-5"] {
        let mut event = event();
        event.attribution.as_mut().unwrap().model = model.into();
        facts(&mut event).speed = ServiceTier::Unknown;
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
    facts(&mut event).request_usage = Some(vec![original, original]);
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
