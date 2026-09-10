use std::path::Path;

use token_tracker::adapters::claude::ClaudeSessionParser;
use token_tracker::application::{
    ParseContext, SessionParser, SessionProvenance, SourceSessionKey, UsageObservation,
    UsageSnapshot, render_terminal_report, summarize_usage,
};
use token_tracker::core::{
    AgentId, AnthropicPricingContext, AnthropicUsage, CacheDetail, EstimateTotal, EstimatedCost,
    ModelAttribution, PricingContext, RawServedValue, RawServiceTier, RecordedCost,
    RequestGranularity, ServiceTier, TierEvidence, Timestamp, TokenCounts, UsageEvent,
};

fn oracle_event() -> UsageEvent {
    let source: String = include_str!("fixtures/claude/snapshots.jsonl")
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

fn facts(event: &mut UsageEvent) -> &mut AnthropicPricingContext {
    event
        .pricing_context
        .as_mut()
        .unwrap()
        .anthropic
        .as_mut()
        .unwrap()
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
    facts(&mut missing_speed).speed = RawServedValue::Missing;
    let mut snapshot = UsageSnapshot::default();
    add_session(
        &mut snapshot,
        "claude",
        "main",
        vec![oracle.clone(), missing_speed],
    );
    let mut conflicting = oracle.clone();
    facts(&mut conflicting).speed = RawServedValue::Value("fast".into());
    add_session(&mut snapshot, "claude", "copy", vec![conflicting]);

    let mut pi = oracle.clone();
    pi.recorded_cost = Some(RecordedCost::from_usd(1.0).unwrap());
    add_session(&mut snapshot, "pi", "main", vec![pi]);
    let mut codex = oracle.clone();
    codex.pricing_context = Some(PricingContext {
        tier: ServiceTier::Standard,
        raw_tier: RawServiceTier::Value("default".into()),
        tier_evidence: TierEvidence::ServedResponse,
        request_granularity: RequestGranularity::ExactSingleRequest,
        cache_detail: CacheDetail::Complete,
        request_usage: None,
        anthropic: None,
    });
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
    assert_eq!(summary.estimates.len(), 2);
    for (agent, cost) in [("claude", 887_500_000), ("codex", 112_500_000)] {
        let totals = &summary.estimates[&AgentId::from(agent)].totals;
        assert_eq!(totals.imported_event_count, 2);
        assert_eq!(totals.priced_event_count, 1);
        assert_eq!(
            totals.cost,
            EstimateTotal::Available(EstimatedCost::from_picodollars(cost))
        );
    }
    let report = render_terminal_report(&summary, &[]);
    assert!(
        report.contains("Total cost: $1.001000 (partial)\n"),
        "{report}"
    );
    for (agent, model, expected) in [
        ("Claude Code", "claude-opus-5", "$0.000888 (partial)"),
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
fn claude_only_zero_and_unpriced_rows_remain_distinct() {
    for (model, expected) in [
        (Some("claude-opus-5"), "$0.000000"),
        (Some("unknown-model"), "unavailable"),
        (None, "unavailable"),
    ] {
        let mut event = oracle_event();
        event.tokens = TokenCounts::default();
        event.attribution = model.map(|model| ModelAttribution {
            provider: "anthropic".into(),
            model: model.into(),
        });
        let AnthropicUsage::Response(component) = &mut facts(&mut event).usage else {
            panic!()
        };
        component.tokens = TokenCounts::default();
        component.cache_creation = None;
        let mut snapshot = UsageSnapshot::default();
        add_session(&mut snapshot, "claude", "main", vec![event]);
        let summary = summarize_usage(&snapshot).unwrap();
        let report = render_terminal_report(&summary, &[]);
        assert!(
            report.contains(&format!("Total cost: {expected}\n")),
            "{report}"
        );
        let label = model.unwrap_or("Unattributed assistants");
        assert!(
            section_row(&report, "Claude Code", label).ends_with(expected),
            "{report}"
        );
    }
}
