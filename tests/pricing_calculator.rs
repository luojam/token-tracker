use token_tracker::application::pricing::calculate_estimate;
use token_tracker::core::{
    CacheDetail, EstimateUnavailableReason, EstimatedCost, ModelAttribution, PricingContext,
    RawServiceTier, RequestGranularity, ServiceTier, TierEvidence, Timestamp, TokenCounts,
    UsageEvent, UsageEventIdentity, UsageKind,
};

fn event() -> UsageEvent {
    UsageEvent {
        identity: UsageEventIdentity {
            agent: "codex".into(),
            adapter_key: "response-v1:example".into(),
        },
        timestamp: Timestamp::from_unix_milliseconds(0),
        kind: UsageKind::Other,
        attribution: Some(ModelAttribution {
            provider: "openai".into(),
            model: "gpt-6-astra".into(),
        }),
        tokens: TokenCounts::default(),
        recorded_cost: None,
        pricing_context: Some(PricingContext {
            tier: ServiceTier::Standard,
            raw_tier: RawServiceTier::Value("default".into()),
            tier_evidence: TierEvidence::RequestedSetting,
            request_granularity: RequestGranularity::ExactSingleRequest,
            cache_detail: CacheDetail::Complete,
        }),
    }
}

#[test]
fn exact_costs_use_disjoint_tokens_and_the_whole_request_band() {
    use ServiceTier::{Fast, Standard};

    // Input, cache read, cache write, output; expected picodollars.
    for (tier, counts, expected) in [
        (Standard, [200, 800, 0, 100], 7_800_000_000),
        (Fast, [200, 800, 0, 100], 15_600_000_000),
        (Standard, [200, 700, 100, 100], 8_950_000_000),
        (Standard, [0, 272_000, 0, 100], 277_000_000_000),
        (Standard, [0, 272_000, 1, 100], 551_525_000_000),
        (Standard, [0, 0, 0, 272_001], 13_600_050_000_000),
        (Standard, [0, 0, 0, 0], 0),
        (Fast, [u64::MAX; 4], u128::from(u64::MAX) * 244_000_000),
    ] {
        let mut event = event();
        let context = event.pricing_context.as_mut().unwrap();
        context.raw_tier =
            RawServiceTier::Value(if tier == Fast { "priority" } else { "default" }.into());
        context.tier = tier;
        event.tokens = TokenCounts {
            input: counts[0],
            cache_read: counts[1],
            cache_write: counts[2],
            output: counts[3],
        };
        assert_eq!(
            calculate_estimate(&event),
            Ok(EstimatedCost::from_picodollars(expected)),
            "{counts:?}",
        );
    }
}

#[test]
fn insufficient_facts_are_unavailable_even_for_zero_tokens() {
    use EstimateUnavailableReason as Reason;

    type Case = (fn(&mut UsageEvent), Reason);
    let cases: [Case; 6] = [
        (|e| e.pricing_context = None, Reason::MissingPricingContext),
        (|e| e.attribution = None, Reason::UnknownAttribution),
        (
            |e| e.pricing_context.as_mut().unwrap().tier = ServiceTier::Unknown,
            Reason::UnknownTier,
        ),
        (
            |e| e.pricing_context.as_mut().unwrap().tier_evidence = TierEvidence::Unknown,
            Reason::UnknownTier,
        ),
        (
            |e| {
                e.pricing_context.as_mut().unwrap().request_granularity =
                    RequestGranularity::AggregateOrUnknown;
                e.pricing_context.as_mut().unwrap().cache_detail = CacheDetail::Incomplete;
            },
            Reason::UnknownRequestGranularity,
        ),
        (
            |e| e.pricing_context.as_mut().unwrap().cache_detail = CacheDetail::Incomplete,
            Reason::IncompleteCacheDetail,
        ),
    ];
    for (change, reason) in cases {
        let mut event = event();
        change(&mut event);
        assert_eq!(calculate_estimate(&event), Err(reason));
    }
}
