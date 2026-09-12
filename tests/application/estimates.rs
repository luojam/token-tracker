use std::path::Path;
use token_tracker::cli::render_terminal_report;

use token_tracker::adapters::claude::ClaudeSessionParser;
use token_tracker::application::{
    ParseContext, SessionParser, SessionProvenance, SourceSessionKey, UsageObservation,
    UsageSnapshot, build_usage_report, summarize_usage,
};
use token_tracker::domain::{
    AnthropicBilling, CacheDetail, EstimateTotal, EstimatedCost, ModelAttribution, OpenAiBilling,
    PricingContext, RecordedCost, RequestBreakdown, ServiceSpeed, ServiceTier, TierEvidence,
    Timestamp, TokenCounts, UsageEvent,
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

fn facts(event: &mut UsageEvent) -> &mut AnthropicBilling {
    let Some(PricingContext::Anthropic(context)) = event.pricing_context.as_mut() else {
        panic!("expected Anthropic billing");
    };
    context
}

fn add_session(snapshot: &mut UsageSnapshot, agent: &str, id: &str, events: Vec<UsageEvent>) {
    let key = SourceSessionKey {
        agent: agent.into(),
        session_id: id.into(),
        source_path: format!("/sessions/{agent}-{id}.jsonl").into(),
    };
    snapshot.sessions.push(SessionProvenance {
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

fn section_row<'a>(report: &'a str, agent: &str, model: &str) -> &'a str {
    report
        .split_once(&format!("{agent} usage:\n"))
        .unwrap()
        .1
        .split("\n\n")
        .next()
        .unwrap()
        .lines()
        .find(|line| line.contains(model))
        .unwrap()
}

#[test]
fn mixed_estimates_use_canonical_events_and_keep_adapter_costs_separate() {
    let oracle = oracle_event();
    let mut missing_speed = oracle.clone();
    missing_speed.identity.adapter_key = "missing-speed".into();
    facts(&mut missing_speed).speed = ServiceSpeed::Unknown;
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
    facts(&mut conflicting).speed = ServiceSpeed::Fast;
    add_session(&mut snapshot, "claude", "copy", vec![conflicting]);

    let mut pi = oracle.clone();
    pi.pricing_context = None;
    pi.recorded_cost = Some(RecordedCost::from_usd(1.0).unwrap());
    add_session(&mut snapshot, "pi", "main", vec![pi]);
    let mut codex = oracle.clone();
    codex.pricing_context = Some(PricingContext::OpenAi(OpenAiBilling {
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

    let summary = summarize_usage(&snapshot).unwrap();
    assert_eq!(summary.totals.estimates.imported_event_count, 4);
    assert_eq!(summary.totals.estimates.priced_event_count, 2);
    let usage_report = build_usage_report(&summary);
    assert_eq!(usage_report.totals.estimates, summary.totals.estimates);
    for (row, source) in usage_report.rows.iter().zip(&summary.breakdown) {
        assert_eq!(row.estimates, source.estimates);
    }
    let report = render_terminal_report(&usage_report, &[]);
    assert!(
        report.contains("Total cost: $2.001000 (partial)\n"),
        "{report}"
    );
    for (agent, model, expected) in [
        ("Claude Code", "claude-opus-5", "$1.000887 (partial)"),
        ("Codex", "claude-opus-5", "unavailable"),
        ("Pi", "claude-opus-5", "$1.000000"),
        ("Codex", "gpt-6-astra", "$0.000113"),
    ] {
        assert!(
            section_row(&report, agent, model).ends_with(expected),
            "{report}"
        );
    }
}

#[test]
fn pricing_follows_billing_provider_for_any_agent_and_retains_all_rate_versions() {
    let anthropic = oracle_event();
    let mut openai = anthropic.clone();
    openai.identity.adapter_key = "openai".into();
    openai.attribution = Some(ModelAttribution {
        provider: "openai".into(),
        model: "gpt-6-astra".into(),
    });
    let original = facts(&mut openai).clone();
    openai.pricing_context = Some(PricingContext::OpenAi(OpenAiBilling {
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
    let summary = summarize_usage(&snapshot).unwrap();
    let estimate = &summary.totals.estimates;
    assert_eq!(estimate.imported_event_count, 3);
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
    let report = render_terminal_report(&build_usage_report(&summary), &[]);
    assert!(report.contains("- Priced events: 2 / 3 without recorded cost\n"));
    assert!(report.contains("- Unpriced (unsupported provider): 1 events\n"));
    for (snapshot, date) in &estimate.rate_snapshots {
        assert!(report.contains(&format!("- Rates: {snapshot} ({date})\n")));
    }
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
fn missing_pricing_facts_still_count_toward_coverage() {
    use token_tracker::domain::EstimateUnavailableReason as Reason;
    let priced = oracle_event();
    let mut missing = priced.clone();
    missing.identity.adapter_key = "missing".into();
    missing.pricing_context = None;
    missing.attribution = None;
    let mut snapshot = UsageSnapshot::default();
    add_session(&mut snapshot, "claude", "main", vec![priced, missing]);
    let summary = summarize_usage(&snapshot).unwrap();
    let totals = &summary.totals.estimates;
    assert_eq!(
        (totals.imported_event_count, totals.priced_event_count),
        (2, 1)
    );
    assert_eq!(
        totals.unavailable_reasons[&Reason::MissingPricingContext],
        1
    );
}
