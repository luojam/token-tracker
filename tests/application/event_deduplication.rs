use token_tracker::application::{
    CostAmount, CostTotal, SessionProvenance, SourceSessionKey, UsageObservation, UsageSnapshot,
    build_usage_report, calculate_usage_summary, deduplicate_events,
};
use token_tracker::domain::{
    AgentId, CacheDetail, EstimateTotal, EstimatedCost, ModelAttribution, OpenAiPricingContext,
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
        source: token_tracker::adapters::files::file_source_key(std::path::Path::new(path)),
    };
    let provenance = SessionProvenance {
        source_path: Some(path.into()),
        name: None,
        working_directory: None,
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
fn shared_event_deduplication_preserves_selected_observations_and_session_memberships() {
    let mut data = snapshot(false);
    let (empty, _) = session("/sessions/empty.jsonl", "empty-session", 300, None, vec![]);
    data.sessions.push(empty);

    let usage = deduplicate_events(&data).unwrap();
    assert_eq!(usage.session_count, 3);
    assert_eq!(
        usage
            .events
            .iter()
            .map(|event| event.canonical.event.identity.adapter_key.as_str())
            .collect::<Vec<_>>(),
        ["branch", "shared", "tool"]
    );
    let shared = &usage.events[1];
    assert_eq!(shared.canonical, &data.observations[0]);
    assert_eq!(shared.sessions, [&data.sessions[1], &data.sessions[0]]);

    let mut reversed = data.clone();
    reversed.sessions.reverse();
    reversed.observations.reverse();
    assert_eq!(deduplicate_events(&reversed).unwrap(), usage);
}

#[test]
fn totals_deduplicate_independently_of_observation_order() {
    let summary = calculate_usage_summary(&snapshot(false)).unwrap();
    assert_eq!(summary, calculate_usage_summary(&snapshot(true)).unwrap());
    assert_eq!(
        summary.totals.tokens,
        TokenCounts {
            input: 16,
            output: 9,
            cache_read: 11,
            cache_write: 13,
        }
    );
    assert_eq!(summary.totals.session_count, 2);
    assert_eq!(summary.totals.unique_usage_event_count, 3);
    assert_eq!(summary.totals.recorded_cost.unwrap().as_usd(), 0.75);
}

#[test]
fn canonical_estimates_keep_whole_observations_and_explicit_pricing_context_coverage() {
    let context = OpenAiPricingContext {
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
            observation.event.pricing_context =
                Some(PricingContext::OpenAi(OpenAiPricingContext {
                    tier: ServiceTier::Fast,
                    tier_evidence: TierEvidence::ServedResponse,
                    ..context.clone()
                }));
        }
    }

    let Some(PricingContext::OpenAi(unknown)) = data.observations[1].event.pricing_context.as_mut()
    else {
        panic!("expected OpenAI pricing context");
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

    let summary = calculate_usage_summary(&data).unwrap();
    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(calculate_usage_summary(&data).unwrap(), summary);

    let estimate = &summary
        .breakdown
        .iter()
        .find(|row| {
            row.agent.as_str() == "codex"
                && row.group == token_tracker::domain::SummaryGroup::ProviderModel(model.clone())
        })
        .unwrap()
        .estimates;
    assert_eq!(summary.totals.tokens.input, 27);
    assert_eq!(summary.totals.recorded_cost.unwrap().as_usd(), 0.5);
    assert_eq!(summary.totals.estimates.estimate_candidate_event_count, 3);
    assert_eq!(
        build_usage_report(&summary).totals.cost,
        CostTotal::Available {
            amount: CostAmount::Usd(0.500395),
            partial: true,
        }
    );

    assert_eq!(estimate.estimate_candidate_event_count, 2);
    assert_eq!(estimate.priced_event_count, 2);
    assert_eq!(estimate.requested_setting_event_count, 0);
    assert_eq!(estimate.served_response_event_count, 1);
    assert_eq!(estimate.assumed_standard_event_count, 1);
    assert_eq!(estimate.assumed_cache_write_event_count, 1);
    assert_eq!(
        estimate.cost,
        EstimateTotal::Available(EstimatedCost::from_picodollars(395_000_000))
    );
    assert_eq!(
        estimate.unavailable_reasons,
        std::collections::BTreeMap::new()
    );
    assert_eq!(
        estimate.tier_event_counts,
        std::collections::BTreeMap::from([(ServiceTier::Standard, 1), (ServiceTier::Fast, 1)])
    );
}

#[test]
fn typed_lineage_and_ambiguous_provenance_use_deterministic_precedence() {
    let mut data = snapshot(false);
    let original = data.sessions[0].key.clone();
    let child = data.sessions[1].key.clone();
    data.sessions[1].parent_session = Some(ParentSession::SessionId(original.session_id.clone()));
    assert_eq!(
        calculate_usage_summary(&data).unwrap(),
        calculate_usage_summary(&snapshot(false)).unwrap()
    );

    data.sessions[0].parent_session = Some(ParentSession::SessionId(child.session_id));
    let expected = calculate_usage_summary(&data).unwrap();
    assert_eq!(expected.totals.tokens.input, 1005);

    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(calculate_usage_summary(&data).unwrap(), expected);
}

#[test]
fn unrelated_copies_use_start_time_then_session_id() {
    let mut data = snapshot(false);
    data.sessions[1].parent_session = None;
    data.sessions[0].started_at = Timestamp::from_unix_milliseconds(50);
    assert_eq!(
        calculate_usage_summary(&data).unwrap().totals.tokens.input,
        16
    );

    data.sessions[0].started_at = data.sessions[1].started_at;
    assert_eq!(
        calculate_usage_summary(&data).unwrap().totals.tokens.input,
        1005
    );

    data.sessions.reverse();
    data.observations.reverse();
    assert_eq!(
        calculate_usage_summary(&data).unwrap().totals.tokens.input,
        1005
    );
}
