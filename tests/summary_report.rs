use std::path::PathBuf;

use token_tracker::application::{
    ImportWarning, SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot,
    render_terminal_report, summarize_usage,
};
use token_tracker::core::{
    AgentId, CacheDetail, EstimateTotal, EstimateUnavailableReason, EstimatedCost,
    ModelAttribution, ParentSession, PricingContext, RawServiceTier, RecordedCost,
    RequestGranularity, ServiceTier, TierEvidence, Timestamp, TokenCounts, UsageEvent,
    UsageEventIdentity, UsageKind,
};

fn event(
    key: &str,
    kind: UsageKind,
    attribution: Option<(&str, &str)>,
    tokens: TokenCounts,
    cost: Option<f64>,
) -> UsageEvent {
    UsageEvent {
        identity: UsageEventIdentity {
            agent: AgentId::from("pi"),
            adapter_key: key.into(),
        },
        timestamp: Timestamp::from_unix_milliseconds(1_000),
        kind,
        attribution: attribution.map(|(provider, model)| ModelAttribution {
            provider: provider.into(),
            model: model.into(),
        }),
        tokens,
        recorded_cost: cost.map(|value| RecordedCost::from_usd(value).unwrap()),
        pricing_context: None,
    }
}

fn session(
    path: &str,
    session_id: &str,
    started_at: i64,
    parent_session: Option<&str>,
    events: Vec<UsageEvent>,
) -> (SessionProvenance, Vec<UsageObservation>) {
    let key = SourceSessionKey {
        agent: "pi".into(),
        session_id: session_id.into(),
        source_path: path.into(),
    };
    let provenance = SessionProvenance {
        key: key.clone(),
        started_at: Timestamp::from_unix_milliseconds(started_at),
        parent_session: parent_session.map(|path| ParentSession::SourcePath(path.into())),
    };
    (
        provenance,
        events
            .into_iter()
            .map(|event| UsageObservation {
                session: key.clone(),
                event,
            })
            .collect(),
    )
}

fn sessions() -> [(SessionProvenance, Vec<UsageObservation>); 2] {
    let original = session(
        "/sessions/original.jsonl",
        "original-session",
        200,
        None,
        vec![
            event(
                "shared",
                UsageKind::Assistant,
                Some(("provider-a", "model-a")),
                TokenCounts {
                    input: 10,
                    output: 2,
                    cache_read: 3,
                    cache_write: 4,
                },
                Some(0.25),
            ),
            event(
                "tool",
                UsageKind::ToolResult,
                None,
                TokenCounts {
                    input: 1,
                    output: 1,
                    cache_read: 1,
                    cache_write: 1,
                },
                None,
            ),
        ],
    );
    let child = session(
        "/sessions/child.jsonl",
        "child-session",
        100,
        Some("/sessions/original.jsonl"),
        vec![
            event(
                "shared",
                UsageKind::Assistant,
                Some(("wrong-provider", "wrong-model")),
                TokenCounts {
                    input: 999,
                    output: 999,
                    cache_read: 999,
                    cache_write: 999,
                },
                Some(9.99),
            ),
            event(
                "branch",
                UsageKind::BranchSummary,
                None,
                TokenCounts {
                    input: 5,
                    output: 6,
                    cache_read: 7,
                    cache_write: 8,
                },
                Some(0.5),
            ),
        ],
    );
    [original, child]
}

fn snapshot(reverse: bool) -> UsageSnapshot {
    let mut records = sessions();
    if reverse {
        records.reverse();
    }
    let mut snapshot = UsageSnapshot::default();
    for (session, observations) in records {
        snapshot.sessions.push(session);
        snapshot.observations.extend(observations);
    }
    snapshot
}

fn summary_for_order(reverse: bool) -> token_tracker::core::UsageSummary {
    summarize_usage(&snapshot(reverse)).unwrap()
}

#[test]
fn summary_reconciles_and_renders_independently_of_observation_order() {
    let summary = summary_for_order(false);
    assert_eq!(summary, summary_for_order(true));
    assert_eq!(
        render_terminal_report(
            &summary,
            &[
                ImportWarning {
                    path: Some(PathBuf::from("/sessions/z-bad.jsonl")),
                    message: "could not parse".into(),
                },
                ImportWarning {
                    path: None,
                    message: "discovery warning".into(),
                },
            ],
        ),
        "Token Tracker — All Time\n\
         \n\
         Total tokens: 49\n\
         Input tokens: 16\n\
         Output tokens: 9\n\
         Cache-read tokens: 11\n\
         Cache-write tokens: 13\n\
         Total cost: $0.750000\n\
         Sessions: 2\n\
         Unique usage events: 3\n\
         \n\
         Usage by provider/model:\n\
         \n\
         Pi usage:\n\
         \x20\x20Provider / model               Input  Output  Cache read  Cache write  Total  Events       Cost\n\
         \x20\x20provider-a / model-a              10       2           3            4     19       1  $0.250000\n\
         \x20\x20Unattributed tool results          1       1           1            1      4       1          -\n\
         \x20\x20Unattributed branch summaries      5       6           7            8     26       1  $0.500000\n\
         \n\
         Warnings (2):\n\
         - discovery warning\n\
         - /sessions/z-bad.jsonl: could not parse\n"
    );
}

#[test]
fn canonical_estimates_keep_whole_observations_and_codex_only_coverage() {
    let context = PricingContext {
        tier: ServiceTier::Standard,
        raw_tier: RawServiceTier::Value("default".into()),
        tier_evidence: TierEvidence::RequestedSetting,
        request_granularity: RequestGranularity::ExactSingleRequest,
        cache_detail: CacheDetail::Complete,
        request_usage: None,
    };
    let model = ModelAttribution {
        provider: "openai".into(),
        model: "gpt-5.6".into(),
    };
    let mut data = snapshot(false);
    for session in &mut data.sessions {
        session.key.agent = "codex".into();
    }
    for observation in &mut data.observations {
        observation.session.agent = "codex".into();
        observation.event.identity.agent = "codex".into();
        if observation.event.identity.adapter_key != "shared" {
            observation.event.attribution = Some(model.clone());
            observation.event.pricing_context = Some(context.clone());
        }
        if observation.session.session_id == "child-session" {
            observation.event.pricing_context = Some(PricingContext {
                tier: ServiceTier::Fast,
                raw_tier: RawServiceTier::Value("priority".into()),
                tier_evidence: TierEvidence::ServedResponse,
                ..context.clone()
            });
        }
    }
    let unknown = data.observations[1].event.pricing_context.as_mut().unwrap();
    unknown.tier = ServiceTier::Unknown;
    unknown.raw_tier = RawServiceTier::Missing;
    unknown.tier_evidence = TierEvidence::Unknown;
    unknown.cache_detail = CacheDetail::Incomplete;
    // The child offers richer context for shared, and conflicting facts for tool.
    let mut conflicting = data.observations[2].clone();
    conflicting.event.identity.adapter_key = "tool".into();
    data.observations.push(conflicting);
    let (pi, mut observations) = sessions().into_iter().next().unwrap();
    observations[0].event.attribution = Some(model.clone());
    data.sessions.push(pi);
    data.observations.extend(observations);

    let summary = summarize_usage(&data).unwrap();
    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(summarize_usage(&data).unwrap(), summary);
    let report = render_terminal_report(&summary, &[]);
    assert!(report.contains("Total cost: $1.000395 (partial)\n"));
    let model_line = report
        .lines()
        .find(|line| line.starts_with("  openai / gpt-5.6 "))
        .unwrap();
    assert!(model_line.ends_with("$0.500395"), "{model_line}");
    let (codex_report, pi_report) = report.split_once("\n\nPi usage:\n").unwrap();
    assert!(codex_report.contains("\n\nCodex usage:\n"));
    for (section, expected) in [
        (codex_report, "openai / gpt-5.6 6 7 8 9 30 2 $0.500395"),
        (pi_report, "openai / gpt-5.6 10 2 3 4 19 1 $0.250000"),
    ] {
        assert!(
            section
                .lines()
                .any(|line| { line.split_whitespace().collect::<Vec<_>>().join(" ") == expected }),
            "{section}"
        );
    }
    let header = |section: &str| {
        section
            .lines()
            .find(|line| line.contains("Provider / model"))
            .unwrap()
            .to_owned()
    };
    assert_eq!(header(codex_report), header(pi_report));
    assert!(pi_report.contains("Unattributed tool results"));
    assert!(!report.contains("API-equivalent estimate"));
    let estimate = summary.estimate.unwrap();
    assert_eq!(summary.totals.tokens.input, 27);
    assert_eq!(summary.totals.recorded_cost.unwrap().as_usd(), 1.0);
    assert_eq!(estimate.totals.imported_event_count, 3);
    assert_eq!(estimate.totals.priced_event_count, 2);
    assert_eq!(estimate.totals.requested_setting_event_count, 0);
    assert_eq!(estimate.totals.served_response_event_count, 1);
    assert_eq!(estimate.totals.assumed_standard_event_count, 1);
    assert_eq!(estimate.totals.assumed_cache_write_event_count, 1);
    assert_eq!(
        estimate.breakdown[0].totals.assumed_cache_write_event_count,
        1
    );
    assert_eq!(estimate.breakdown[0].totals.assumed_standard_event_count, 1);
    assert_eq!(
        estimate.totals.cost,
        EstimateTotal::Available(EstimatedCost::from_picodollars(395_000_000))
    );
    assert_eq!(
        estimate.totals.unavailable_reasons,
        std::collections::BTreeMap::from([(EstimateUnavailableReason::MissingPricingContext, 1)])
    );
    assert_eq!(
        estimate
            .breakdown
            .iter()
            .map(|row| (&row.tier, row.totals.priced_event_count))
            .collect::<Vec<_>>(),
        vec![
            (&ServiceTier::Standard, 1),
            (&ServiceTier::Fast, 1),
            (&ServiceTier::Unknown, 0)
        ]
    );

    data.observations.reverse();
    data.observations.truncate(1);
    let unpriced = summarize_usage(&data).unwrap().estimate.unwrap();
    assert_eq!(unpriced.totals.cost, EstimateTotal::Unavailable);
    let event = &mut data.observations[0].event;
    event.attribution = Some(model);
    event.pricing_context = Some(context);
    event.tokens = TokenCounts::default();
    let zero = summarize_usage(&data).unwrap().estimate.unwrap();
    assert_eq!(
        zero.totals.cost,
        EstimateTotal::Available(EstimatedCost::default())
    );
    data.observations.clear();
    assert!(summarize_usage(&data).unwrap().estimate.is_none());
}

#[test]
fn typed_lineage_and_ambiguous_provenance_use_deterministic_precedence() {
    let mut data = snapshot(false);
    let original = data.sessions[0].key.clone();
    let child = data.sessions[1].key.clone();
    data.sessions[1].parent_session = Some(ParentSession::SessionId(original.session_id.clone()));
    assert_eq!(summarize_usage(&data).unwrap(), summary_for_order(false));

    // Cycles are treated as ambiguous, never as an import-order tie breaker.
    data.sessions[0].parent_session = Some(ParentSession::SessionId(child.session_id));
    let expected = summarize_usage(&data).unwrap();
    assert_eq!(expected.totals.tokens.input, 1005);
    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(summarize_usage(&data).unwrap(), expected);
}
