use std::path::PathBuf;
use token_tracker::cli::render_terminal_report;

use token_tracker::application::{
    ImportWarning, SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot,
    build_usage_report, summarize_usage,
};
use token_tracker::domain::{
    AgentId, CacheDetail, EstimateTotal, EstimatedCost, ModelAttribution, OpenAiBilling,
    ParentSession, PricingContext, RecordedCost, RequestBreakdown, ServiceTier, TierEvidence,
    Timestamp, TokenCounts, UsageEvent, UsageEventIdentity, UsageKind,
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

#[test]
fn summary_reconciles_and_renders_independently_of_observation_order() {
    let summary = summarize_usage(&snapshot(false)).unwrap();
    assert_eq!(summary, summarize_usage(&snapshot(true)).unwrap());
    assert_eq!(
        render_terminal_report(
            &build_usage_report(&summary),
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
        include_str!("../fixtures/all_time_report.txt")
    );
}

#[test]
fn canonical_estimates_keep_whole_observations_and_explicit_billing_coverage() {
    let context = OpenAiBilling {
        tier: ServiceTier::Standard,

        tier_evidence: TierEvidence::RequestedSetting,
        requests: RequestBreakdown::SingleRequest,
        cache_detail: CacheDetail::Complete,
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
            observation.event.recorded_cost = None;
            observation.event.attribution = Some(model.clone());
            observation.event.pricing_context = Some(PricingContext::OpenAi(context.clone()));
        }
        if observation.session.session_id == "child-session" {
            observation.event.pricing_context = Some(PricingContext::OpenAi(OpenAiBilling {
                tier: ServiceTier::Fast,

                tier_evidence: TierEvidence::ServedResponse,
                ..context.clone()
            }));
        }
    }
    let Some(PricingContext::OpenAi(unknown)) = data.observations[1].event.pricing_context.as_mut()
    else {
        panic!("expected OpenAI billing");
    };
    unknown.tier = ServiceTier::Unknown;

    unknown.tier_evidence = TierEvidence::Unknown;
    unknown.cache_detail = CacheDetail::Incomplete;
    // The child offers richer context for shared, and conflicting facts for tool.
    let mut conflicting = data.observations[2].clone();
    conflicting.event.identity.adapter_key = "tool".into();
    data.observations.push(conflicting);
    let (pi, mut observations) = sessions().into_iter().next().unwrap();
    observations[0].event.attribution = Some(model.clone());
    observations[0].event.pricing_context = Some(PricingContext::OpenAi(context));
    data.sessions.push(pi);
    data.observations.extend(observations);

    let summary = summarize_usage(&data).unwrap();
    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(summarize_usage(&data).unwrap(), summary);
    let estimate = summary.estimates[&AgentId::from("codex")].clone();
    assert_eq!(summary.totals.tokens.input, 27);
    assert_eq!(summary.totals.recorded_cost.unwrap().as_usd(), 0.5);
    assert_eq!(summary.estimates[&"pi".into()].totals.imported_event_count, 1);
    let report = render_terminal_report(&build_usage_report(&summary), &[]);
    assert!(report.contains("Total cost: $0.500395 (partial)\n"));
    assert_eq!(estimate.totals.imported_event_count, 2);
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
        std::collections::BTreeMap::new()
    );
    assert_eq!(
        estimate
            .breakdown
            .iter()
            .map(|row| (&row.tier, row.totals.priced_event_count))
            .collect::<Vec<_>>(),
        vec![(&ServiceTier::Standard, 1), (&ServiceTier::Fast, 1)]
    );
}

#[test]
fn typed_lineage_and_ambiguous_provenance_use_deterministic_precedence() {
    let mut data = snapshot(false);
    let original = data.sessions[0].key.clone();
    let child = data.sessions[1].key.clone();
    data.sessions[1].parent_session = Some(ParentSession::SessionId(original.session_id.clone()));
    assert_eq!(
        summarize_usage(&data).unwrap(),
        summarize_usage(&snapshot(false)).unwrap()
    );

    data.sessions[0].parent_session = Some(ParentSession::SessionId(child.session_id));
    let expected = summarize_usage(&data).unwrap();
    assert_eq!(expected.totals.tokens.input, 1005);
    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(summarize_usage(&data).unwrap(), expected);
}

#[test]
fn unrelated_copies_use_start_time_then_source_path() {
    let mut data = snapshot(false);
    data.sessions[1].parent_session = None;
    data.sessions[0].started_at = Timestamp::from_unix_milliseconds(50);
    assert_eq!(summarize_usage(&data).unwrap().totals.tokens.input, 16);
    data.sessions[0].started_at = data.sessions[1].started_at;
    assert_eq!(summarize_usage(&data).unwrap().totals.tokens.input, 1005);
    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(summarize_usage(&data).unwrap().totals.tokens.input, 1005);
}
