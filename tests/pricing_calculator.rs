use token_tracker::domain::{
    CacheDetail, EstimateUnavailableReason, EstimatedCost, KnownRequests, ModelAttribution,
    OpenAiBilling, PricingContext, RequestBreakdown, ServiceTier, TierEvidence, Timestamp,
    TokenCounts, UsageEvent, UsageEventIdentity, UsageKind,
};
use token_tracker::pricing::openai::{self as pricing, MissingCacheWritePolicy};

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
        pricing_context: Some(PricingContext::OpenAi(OpenAiBilling {
            tier: ServiceTier::Standard,

            tier_evidence: TierEvidence::RequestedSetting,
            requests: RequestBreakdown::SingleRequest,
            cache_detail: CacheDetail::Complete,
        })),
    }
}

#[test]
fn missing_pricing_facts_remain_in_estimate_coverage() {
    let priced = event();
    let mut missing = priced.clone();
    missing.identity.adapter_key = "missing".into();
    missing.pricing_context = None;
    for attribution in [
        None,
        Some(ModelAttribution {
            provider: "unsupported".into(),
            model: "unknown".into(),
        }),
    ] {
        missing.attribution = attribution;
        let summary = token_tracker::domain::UsageSummary {
            estimates: token_tracker::pricing::summarize_estimates([&priced, &missing]),
            ..Default::default()
        };
        let totals = &summary.estimates[&priced.identity.agent].totals;
        assert_eq!(totals.imported_event_count, 2);
        assert_eq!(totals.priced_event_count, 1);
        let reason = if missing.attribution.is_some() {
            EstimateUnavailableReason::UnsupportedProvider
        } else {
            EstimateUnavailableReason::MissingPricingContext
        };
        assert_eq!(totals.unavailable_reasons[&reason], 1);
        let report = token_tracker::cli::render_terminal_report(&summary, &[]);
        assert!(report.contains("Total cost: $0.000000 (partial)\n"));
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
        let context = facts(&mut event);
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
            for tier_evidence in [TierEvidence::Unknown, TierEvidence::RequestedSetting] {
                let context = facts(&mut event);
                context.tier = ServiceTier::Unknown;
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
            |e| facts(e).tier = ServiceTier::Unsupported("flex".into()),
            Reason::UnsupportedTier,
        ),
        (
            |e| facts(e).tier_evidence = TierEvidence::Unknown,
            Reason::UnknownTier,
        ),
        (
            |e| {
                facts(e).requests = RequestBreakdown::AggregateOrUnknown;
                facts(e).cache_detail = CacheDetail::Incomplete;
            },
            Reason::IncompleteCacheDetail,
        ),
        (
            |e| facts(e).cache_detail = CacheDetail::Incomplete,
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
            facts(&mut event).tier = tier;
            event.tokens = TokenCounts {
                input: 100_000,
                cache_read: 171_999,
                cache_write: 1,
                output: 400_000,
            };
            let expected = calculate_estimate(&event).unwrap();
            facts(&mut event).requests = RequestBreakdown::AggregateOrUnknown;
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
                facts(&mut event).requests = RequestBreakdown::SingleRequest;
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
        facts(&mut event).cache_detail = CacheDetail::Incomplete;
        let result = calculate_estimate(&event);
        if ["gpt-5.6-sol", "gpt-6-astra"].contains(&model) {
            assert_eq!(
                result,
                Err(EstimateUnavailableReason::IncompleteCacheDetail)
            );
        } else {
            event.tokens.input = 0;
            event.tokens.cache_write = 100;
            facts(&mut event).cache_detail = CacheDetail::Complete;
            assert_eq!(result, calculate_estimate(&event));
            assert!(result.is_ok());
        }
        facts(&mut event).tier = ServiceTier::Unknown;
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
        let context = facts(&mut event);
        context.requests = RequestBreakdown::AggregateOrUnknown;
        context.cache_detail = CacheDetail::Incomplete;
        assert_eq!(
            calculate_estimate(&event),
            Err(EstimateUnavailableReason::UnknownRequestGranularity)
        );
        facts(&mut event).requests =
            RequestBreakdown::KnownRequests(KnownRequests::from_vec(requests).unwrap());
        assert_eq!(
            calculate_estimate(&event),
            Ok(EstimatedCost::from_picodollars(expected))
        );
        event.tokens.input += 1;
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
    let context = facts(&mut event);
    context.cache_detail = CacheDetail::Incomplete;
    context.requests = RequestBreakdown::KnownRequests(KnownRequests::from_vec(requests).unwrap());
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

    facts(&mut event).cache_detail = CacheDetail::Complete;
    let known = pricing::calculate_estimate(&event, MissingCacheWritePolicy::TreatAsInput).unwrap();
    assert_eq!(known.cost, estimate.cost);
    assert!(!known.assumed_cache_writes_as_input);
    assert_eq!(calculate_estimate(&event), Ok(known.cost));

    facts(&mut event).cache_detail = CacheDetail::Incomplete;
    event.attribution.as_mut().unwrap().model = "gpt-5.5".into();
    assert!(
        !pricing::calculate_estimate(&event, MissingCacheWritePolicy::TreatAsInput)
            .unwrap()
            .assumed_cache_writes_as_input
    );

    facts(&mut event).requests = RequestBreakdown::AggregateOrUnknown;
    assert_eq!(
        pricing::calculate_estimate(&event, MissingCacheWritePolicy::TreatAsInput),
        Err(EstimateUnavailableReason::UnknownRequestGranularity),
    );
}

fn facts(event: &mut UsageEvent) -> &mut OpenAiBilling {
    let Some(PricingContext::OpenAi(context)) = event.pricing_context.as_mut() else {
        panic!("expected OpenAI billing");
    };
    context
}
