use std::path::Path;
use token_tracker::adapters::files::{ParseContext, SessionParser};

use token_tracker::adapters::claude::ClaudeSessionParser;
use token_tracker::application::{
    CostAmount, CostTotal, SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot,
    build_usage_report, calculate_usage_summary,
};
use token_tracker::domain::{
    AnthropicPricingContext, CacheDetail, EstimateTotal, EstimatedCost, ModelAttribution,
    OpenAiPricingContext, PricingContext, RecordedCost, RequestBreakdown, ServiceSpeed,
    ServiceTier, TierEvidence, Timestamp, TokenCounts, UsageEvent,
};

fn oracle_event() -> UsageEvent {
    let source: String = include_str!("../fixtures/claude/snapshots.jsonl")
        .split_inclusive('\n')
        .take(4)
        .collect();
    ClaudeSessionParser::new()
        .parse(
            &mut source.as_bytes(),
            ParseContext {
                source_path: Path::new(
                    "/projects/example/11111111-1111-4111-8111-111111111111.jsonl",
                ),
            },
        )
        .unwrap()
        .events
        .remove(0)
}

fn anthropic_context_mut(event: &mut UsageEvent) -> &mut AnthropicPricingContext {
    let Some(PricingContext::Anthropic(context)) = event.pricing_context.as_mut() else {
        panic!("expected Anthropic pricing context");
    };
    context
}

fn add_session(snapshot: &mut UsageSnapshot, agent: &str, id: &str, events: Vec<UsageEvent>) {
    let key = SourceSessionKey {
        agent: agent.into(),
        session_id: id.into(),
        source: token_tracker::application::SourceKey(format!("{agent}:{id}").into_bytes()),
    };
    snapshot.sessions.push(SessionProvenance {
        source_path: None,
        name: None,
        working_directory: None,
        key: key.clone(),
        started_at: Timestamp::from_unix_milliseconds(snapshot.sessions.len() as i64),
        parent_session: None,
    });
    snapshot
        .observations
        .extend(events.into_iter().map(|mut event| {
            event.identity.agent = agent.into();
            UsageObservation {
                session: key.clone(),
                event,
            }
        }));
}

#[test]
fn mixed_estimates_use_canonical_events_and_keep_adapter_costs_separate() {
    let oracle = oracle_event();
    let mut missing_speed = oracle.clone();
    missing_speed.identity.adapter_key = "missing-speed".into();
    anthropic_context_mut(&mut missing_speed).speed = ServiceSpeed::Unknown;

    let mut recorded = oracle.clone();
    recorded.identity.adapter_key = "recorded".into();
    recorded.recorded_cost = Some(RecordedCost::from_usd(1.0).unwrap());

    let mut snapshot = UsageSnapshot::default();
    add_session(
        &mut snapshot,
        "claude",
        "main",
        vec![oracle.clone(), missing_speed, recorded],
    );

    let mut conflicting = oracle.clone();
    anthropic_context_mut(&mut conflicting).speed = ServiceSpeed::Fast;
    add_session(&mut snapshot, "claude", "copy", vec![conflicting]);

    let mut pi = oracle.clone();
    pi.pricing_context = None;
    pi.recorded_cost = Some(RecordedCost::from_usd(1.0).unwrap());
    add_session(&mut snapshot, "pi", "main", vec![pi]);

    let mut codex = oracle.clone();
    codex.pricing_context = Some(PricingContext::OpenAi(OpenAiPricingContext {
        tier: ServiceTier::Standard,
        tier_evidence: TierEvidence::ServedResponse,
        requests: RequestBreakdown::SingleRequest,
        cache_detail: CacheDetail::Complete,
    }));
    let unsupported = codex.clone();
    codex.identity.adapter_key = "openai-response".into();
    codex.attribution = Some(ModelAttribution {
        provider: "openai".into(),
        model: "gpt-6-astra".into(),
    });
    codex.tokens = TokenCounts {
        input: 10,
        cache_write: 1,
        ..TokenCounts::default()
    };
    add_session(&mut snapshot, "codex", "main", vec![codex, unsupported]);

    let summary = calculate_usage_summary(&snapshot).unwrap();
    assert_eq!(summary.totals.estimates.estimate_candidate_event_count, 4);
    assert_eq!(summary.totals.estimates.priced_event_count, 2);

    let usage_report = build_usage_report(&summary);
    assert_eq!(usage_report.totals.estimates, summary.totals.estimates);
    for (row, source) in usage_report.rows.iter().zip(&summary.breakdown) {
        assert_eq!(row.estimates, source.estimates);
    }
    assert_eq!(
        usage_report.totals.cost,
        CostTotal::Available {
            amount: CostAmount::Usd(2.001),
            partial: true,
        }
    );

    for (agent, model, expected) in [
        (
            "claude",
            "claude-opus-5",
            CostTotal::Available {
                amount: CostAmount::Usd(1.0008875),
                partial: true,
            },
        ),
        ("codex", "claude-opus-5", CostTotal::Unavailable),
        (
            "pi",
            "claude-opus-5",
            CostTotal::Available {
                amount: CostAmount::Usd(1.0),
                partial: false,
            },
        ),
        (
            "codex",
            "gpt-6-astra",
            CostTotal::Available {
                amount: CostAmount::Estimated(EstimatedCost::from_picodollars(112_500_000)),
                partial: false,
            },
        ),
    ] {
        let row = usage_report
            .rows
            .iter()
            .find(|row| {
                row.agent.as_str() == agent
                    && matches!(
                        &row.group,
                        token_tracker::domain::SummaryGroup::ProviderModel(attribution)
                            if attribution.model == model
                    )
            })
            .unwrap();
        assert_eq!(row.cost, expected);
    }
}

#[test]
fn pricing_follows_context_provider_for_any_agent_and_retains_all_rate_versions() {
    let anthropic = oracle_event();
    let mut openai = anthropic.clone();
    openai.identity.adapter_key = "openai".into();
    openai.attribution = Some(ModelAttribution {
        provider: "openai".into(),
        model: "gpt-6-astra".into(),
    });
    let original = anthropic_context_mut(&mut openai).clone();
    openai.pricing_context = Some(PricingContext::OpenAi(OpenAiPricingContext {
        tier: original.tier,
        tier_evidence: original.tier_evidence,
        requests: original.requests,
        cache_detail: CacheDetail::Complete,
    }));

    let mut unknown = anthropic.clone();
    unknown.identity.adapter_key = "unknown".into();
    unknown.pricing_context = None;
    unknown.attribution.as_mut().unwrap().provider = "unknown-provider".into();

    let mut snapshot = UsageSnapshot::default();
    add_session(
        &mut snapshot,
        "another-agent",
        "main",
        vec![anthropic, openai, unknown],
    );

    let summary = calculate_usage_summary(&snapshot).unwrap();
    let estimate = &summary.totals.estimates;
    assert_eq!(estimate.estimate_candidate_event_count, 3);
    assert_eq!(estimate.priced_event_count, 2);
    assert_eq!(
        estimate.cost,
        EstimateTotal::Available(EstimatedCost::from_picodollars(2_587_500_000))
    );
    assert_eq!(
        estimate.unavailable_reasons
            [&token_tracker::domain::EstimateUnavailableReason::UnsupportedProvider],
        1
    );
    assert_eq!(estimate.rate_snapshots.len(), 2);
    assert!(
        estimate
            .rate_snapshots
            .contains_key(token_tracker::pricing::openai::SNAPSHOT_ID)
    );
    assert!(
        estimate
            .rate_snapshots
            .contains_key(token_tracker::pricing::anthropic::SNAPSHOT_ID)
    );
}

#[test]
fn missing_pricing_context_still_count_toward_coverage() {
    use token_tracker::domain::EstimateUnavailableReason as Reason;

    let priced = oracle_event();
    let mut missing = priced.clone();
    missing.identity.adapter_key = "missing".into();
    missing.pricing_context = None;
    missing.attribution = None;

    let mut snapshot = UsageSnapshot::default();
    add_session(&mut snapshot, "claude", "main", vec![priced, missing]);

    let summary = calculate_usage_summary(&snapshot).unwrap();
    let totals = &summary.totals.estimates;
    assert_eq!(
        (
            totals.estimate_candidate_event_count,
            totals.priced_event_count
        ),
        (2, 1)
    );
    assert_eq!(
        totals.unavailable_reasons[&Reason::MissingPricingContext],
        1
    );
}
