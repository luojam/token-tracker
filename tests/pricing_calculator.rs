use token_tracker::application::pricing::{self, MissingCacheWritePolicy};
use token_tracker::core::{
    CacheDetail, EstimateUnavailableReason, EstimatedCost, ModelAttribution, PricingContext,
    RawServiceTier, RequestGranularity, ServiceTier, TierEvidence, Timestamp, TokenCounts,
    UsageEvent, UsageEventIdentity, UsageKind,
};

fn calculate_estimate(event: &UsageEvent) -> Result<EstimatedCost, EstimateUnavailableReason> {
    pricing::calculate_estimate(event, MissingCacheWritePolicy::Reject)
        .map(|estimate| estimate.cost)
}

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
            request_usage: None,
            anthropic: None,
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
fn unknown_tiers_use_standard_rates_without_changing_usage_facts() {
    for model in ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.5", "gpt-5.4-mini"] {
        for input in [100, 300_000] {
            let mut event = event();
            event.attribution.as_mut().unwrap().model = model.into();
            event.tokens = TokenCounts {
                input,
                cache_read: 200,
                cache_write: 50,
                output: 25,
            };
            let standard = calculate_estimate(&event).unwrap();
            for (raw_tier, tier_evidence) in [
                (RawServiceTier::Missing, TierEvidence::Unknown),
                (RawServiceTier::Missing, TierEvidence::RequestedSetting),
                (RawServiceTier::Null, TierEvidence::RequestedSetting),
                (
                    RawServiceTier::Value("auto".into()),
                    TierEvidence::RequestedSetting,
                ),
            ] {
                let context = event.pricing_context.as_mut().unwrap();
                context.tier = ServiceTier::Unknown;
                context.raw_tier = raw_tier;
                context.tier_evidence = tier_evidence;
                let original = event.clone();
                assert_eq!(calculate_estimate(&event), Ok(standard));
                assert_eq!(event, original);
            }
        }
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
            |e| e.pricing_context.as_mut().unwrap().tier = ServiceTier::Unsupported("flex".into()),
            Reason::UnsupportedTier,
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
            Reason::IncompleteCacheDetail,
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

#[test]
fn aggregates_are_priced_only_when_request_partition_cannot_change_the_cost() {
    for model in ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.5", "gpt-5.4-mini"] {
        for tier in [ServiceTier::Standard, ServiceTier::Fast] {
            let mut event = event();
            event.attribution.as_mut().unwrap().model = model.into();
            event.pricing_context.as_mut().unwrap().tier = tier;
            event.tokens = TokenCounts {
                input: 100_000,
                cache_read: 171_999,
                cache_write: 1,
                output: 400_000,
            };
            let expected = calculate_estimate(&event).unwrap();
            event.pricing_context.as_mut().unwrap().request_granularity =
                RequestGranularity::AggregateOrUnknown;
            assert_eq!(calculate_estimate(&event), Ok(expected));

            let mut part = event.clone();
            part.tokens = TokenCounts {
                input: 100_000,
                ..TokenCounts::default()
            };
            event.tokens.input = 0;
            assert_eq!(
                calculate_estimate(&event)
                    .unwrap()
                    .checked_add(calculate_estimate(&part).unwrap()),
                Some(expected),
            );

            event.tokens.input = 100_001;
            if model == "gpt-5.4-mini" {
                let aggregate = calculate_estimate(&event).unwrap();
                event.pricing_context.as_mut().unwrap().request_granularity =
                    RequestGranularity::ExactSingleRequest;
                assert_eq!(calculate_estimate(&event), Ok(aggregate));
            } else {
                assert_eq!(
                    calculate_estimate(&event),
                    Err(EstimateUnavailableReason::UnknownRequestGranularity),
                    "{model}",
                );
            }
        }
    }
}

#[test]
fn missing_cache_write_detail_is_usable_only_without_a_write_premium() {
    for model in ["gpt-5.4-mini", "gpt-5.5", "gpt-5.6-sol", "gpt-6-astra"] {
        let mut event = event();
        event.attribution.as_mut().unwrap().model = model.into();
        event.tokens = TokenCounts {
            input: 100,
            output: 10,
            ..TokenCounts::default()
        };
        event.pricing_context.as_mut().unwrap().cache_detail = CacheDetail::Incomplete;
        let result = calculate_estimate(&event);
        if ["gpt-5.6-sol", "gpt-6-astra"].contains(&model) {
            assert_eq!(
                result,
                Err(EstimateUnavailableReason::IncompleteCacheDetail)
            );
        } else {
            event.tokens.input = 0;
            event.tokens.cache_write = 100;
            event.pricing_context.as_mut().unwrap().cache_detail = CacheDetail::Complete;
            assert_eq!(result, calculate_estimate(&event));
            assert!(result.is_ok());
        }
        event.pricing_context.as_mut().unwrap().tier = ServiceTier::Unknown;
        assert_eq!(calculate_estimate(&event), result);
    }
}

#[test]
fn request_breakdowns_apply_context_bands_per_request_and_require_complete_totals() {
    for (inputs, expected) in [
        ([200_000, 200_000], 2_000_900_000_000),
        ([200_000, 300_000], 4_001_200_000_000),
        ([272_000, 272_001], 4_081_210_000_000),
    ] {
        let mut event = event();
        event.attribution.as_mut().unwrap().model = "gpt-5.5".into();
        let requests = vec![
            TokenCounts {
                input: inputs[0],
                output: 10,
                ..TokenCounts::default()
            },
            TokenCounts {
                input: inputs[1],
                output: 20,
                ..TokenCounts::default()
            },
        ];
        event.tokens = requests[0].checked_add(requests[1]).unwrap();
        let context = event.pricing_context.as_mut().unwrap();
        context.request_granularity = RequestGranularity::AggregateOrUnknown;
        context.cache_detail = CacheDetail::Incomplete;
        assert_eq!(
            calculate_estimate(&event),
            Err(EstimateUnavailableReason::UnknownRequestGranularity)
        );
        event.pricing_context.as_mut().unwrap().request_usage = Some(requests);
        assert_eq!(
            calculate_estimate(&event),
            Ok(EstimatedCost::from_picodollars(expected))
        );
        event.tokens.input += 1;
        assert_eq!(
            calculate_estimate(&event),
            Err(EstimateUnavailableReason::UnknownRequestGranularity)
        );
        event.tokens = TokenCounts::default();
        event.pricing_context.as_mut().unwrap().request_usage = Some(vec![]);
        assert_eq!(
            calculate_estimate(&event),
            Err(EstimateUnavailableReason::UnknownRequestGranularity)
        );
    }
}

#[test]
fn explicit_cache_policy_prices_unresolved_input_without_reclassifying_known_writes() {
    let mut event = event();
    event.attribution.as_mut().unwrap().model = "gpt-5.6-sol".into();
    let requests = vec![
        TokenCounts {
            input: 2_000,
            cache_read: 8_000,
            output: 500,
            cache_write: 0,
        },
        TokenCounts {
            input: 300_000,
            cache_read: 8_000,
            output: 500,
            cache_write: 1_000,
        },
    ];
    event.tokens = requests[0].checked_add(requests[1]).unwrap();
    let context = event.pricing_context.as_mut().unwrap();
    context.cache_detail = CacheDetail::Incomplete;
    context.request_granularity = RequestGranularity::AggregateOrUnknown;
    context.request_usage = Some(requests);
    let original = event.clone();
    assert_eq!(
        calculate_estimate(&event),
        Err(EstimateUnavailableReason::IncompleteCacheDetail)
    );
    let estimate =
        pricing::calculate_estimate(&event, MissingCacheWritePolicy::TreatAsInput).unwrap();
    assert_eq!(estimate.cost.as_picodollars(), 2_452_600_000_000);
    assert!(estimate.assumed_cache_writes_as_input);
    assert_eq!(event, original);

    event.pricing_context.as_mut().unwrap().cache_detail = CacheDetail::Complete;
    let known = pricing::calculate_estimate(&event, MissingCacheWritePolicy::TreatAsInput).unwrap();
    assert_eq!(known.cost, estimate.cost);
    assert!(!known.assumed_cache_writes_as_input);
    assert_eq!(calculate_estimate(&event), Ok(known.cost));

    event.pricing_context.as_mut().unwrap().cache_detail = CacheDetail::Incomplete;
    event.attribution.as_mut().unwrap().model = "gpt-5.5".into();
    assert!(
        !pricing::calculate_estimate(&event, MissingCacheWritePolicy::TreatAsInput)
            .unwrap()
            .assumed_cache_writes_as_input
    );

    event.pricing_context.as_mut().unwrap().request_usage = None;
    assert_eq!(
        pricing::calculate_estimate(&event, MissingCacheWritePolicy::TreatAsInput),
        Err(EstimateUnavailableReason::UnknownRequestGranularity),
    );
}
