use token_tracker::application::{CostAmount, CostTotal, ImportWarning, build_usage_report};
use token_tracker::cli::render_terminal_report;
use token_tracker::domain::{
    EstimateBreakdown, EstimateSummary, EstimateTotal, EstimateTotals, EstimatedCost,
    ModelAttribution, RecordedCost, ServiceTier, SummaryBreakdown, SummaryGroup, TokenCounts,
    UsageSummary,
};

fn summary(cost: EstimateTotal, priced: u64) -> UsageSummary {
    UsageSummary {
        estimates: std::collections::BTreeMap::from([(
            "codex".into(),
            EstimateSummary {
                rate_snapshots: std::collections::BTreeMap::from([(
                    "test-snapshot".into(),
                    "2026-09-07".into(),
                )]),
                totals: EstimateTotals {
                    cost,
                    imported_event_count: 1,
                    priced_event_count: priced,
                    served_response_event_count: priced,
                    ..EstimateTotals::default()
                },
                breakdown: vec![],
            },
        )]),
        ..UsageSummary::default()
    }
}

#[test]
fn cost_states_stay_distinct_without_hiding_import_warnings() {
    for (cost, priced, label) in [
        (EstimateTotal::Unavailable, 0, "unavailable"),
        (
            EstimateTotal::Available(EstimatedCost::default()),
            1,
            "$0.000000",
        ),
        (
            EstimateTotal::Available(EstimatedCost::from_picodollars(1)),
            1,
            "<$0.000001",
        ),
        (
            EstimateTotal::Overflow,
            1,
            "unavailable (arithmetic overflow)",
        ),
    ] {
        let report = render_terminal_report(
            &build_usage_report(&summary(cost, priced)),
            &[ImportWarning {
                path: None,
                message: "could not parse".into(),
            }],
        );
        assert!(report.contains(&format!("Total cost: {label}\n")));
        assert!(!report.contains("(partial)"));
        assert!(!report.contains("Requested settings do not confirm"));
        assert!(report.ends_with("Warnings (1):\n- could not parse\n"));
    }
}

#[test]
fn estimate_labels_escape_source_control_characters() {
    let mut summary = summary(EstimateTotal::Unavailable, 0);
    let estimate = summary.estimates.get_mut(&"codex".into()).unwrap();
    let attribution = ModelAttribution {
        provider: "custom\nprovider".into(),
        model: "model\t".into(),
    };
    estimate.breakdown.push(EstimateBreakdown {
        group: SummaryGroup::ProviderModel(attribution.clone()),
        tier: ServiceTier::Unsupported("priority\r\u{1b}".into()),
        totals: estimate.totals.clone(),
    });
    summary.breakdown.push(SummaryBreakdown {
        agent: "codex".into(),
        group: SummaryGroup::ProviderModel(attribution),
        tokens: TokenCounts::default(),
        recorded_cost: None,
        unique_usage_event_count: 1,
    });
    let report = render_terminal_report(&build_usage_report(&summary), &[]);
    assert!(report.contains("  custom\\nprovider / model\\t "));
    assert!(report.contains("  unavailable\n"));
    assert!(!report.chars().any(|c| c.is_control() && c != '\n'));
}

#[test]
fn model_costs_combine_tiers_and_recorded_costs_without_mixing_providers_or_agents() {
    let mut summary = summary(
        EstimateTotal::Available(EstimatedCost::from_picodollars(9_000_000_000_000)),
        3,
    );
    summary.totals.recorded_cost = Some(RecordedCost::from_usd(1.0).unwrap());
    summary
        .estimates
        .get_mut(&"codex".into())
        .unwrap()
        .totals
        .imported_event_count = 4;
    for (provider, recorded) in [("openai", Some(1.0)), ("other", None)] {
        summary.breakdown.push(SummaryBreakdown {
            agent: "codex".into(),
            group: SummaryGroup::ProviderModel(ModelAttribution {
                provider: provider.into(),
                model: "model".into(),
            }),
            tokens: TokenCounts::default(),
            recorded_cost: recorded.map(|cost| RecordedCost::from_usd(cost).unwrap()),
            unique_usage_event_count: 2,
        });
    }
    for (provider, tier, cost) in [
        ("openai", ServiceTier::Standard, Some(2_000_000_000_000)),
        ("openai", ServiceTier::Fast, Some(3_000_000_000_000)),
        ("other", ServiceTier::Standard, Some(4_000_000_000_000)),
        ("other", ServiceTier::Unsupported("custom".into()), None),
    ] {
        summary
            .estimates
            .get_mut(&"codex".into())
            .unwrap()
            .breakdown
            .push(EstimateBreakdown {
                group: SummaryGroup::ProviderModel(ModelAttribution {
                    provider: provider.into(),
                    model: "model".into(),
                }),
                tier,
                totals: EstimateTotals {
                    cost: cost.map_or(EstimateTotal::Unavailable, |cost| {
                        EstimateTotal::Available(EstimatedCost::from_picodollars(cost))
                    }),
                    imported_event_count: 1,
                    priced_event_count: u64::from(cost.is_some()),
                    ..EstimateTotals::default()
                },
            });
    }
    let mut other_agent = summary.breakdown[0].clone();
    other_agent.agent = "pi".into();
    other_agent.recorded_cost = None;
    summary.breakdown.push(other_agent);

    let report = build_usage_report(&summary);
    assert_eq!(
        report.totals.cost,
        CostTotal::Available {
            amount: CostAmount::Usd(10.0),
            partial: true
        }
    );
    assert_eq!(
        report.rows[0].cost,
        CostTotal::Available {
            amount: CostAmount::Usd(6.0),
            partial: false
        }
    );
    assert_eq!(
        report.rows[1].cost,
        CostTotal::Available {
            amount: CostAmount::Estimated(EstimatedCost::from_picodollars(4_000_000_000_000)),
            partial: true
        }
    );
    assert_eq!(report.rows[2].cost, CostTotal::Absent);
}

#[test]
fn combined_adapter_cost_overflow_invalidates_the_total() {
    for claude_cost in [
        EstimateTotal::Overflow,
        EstimateTotal::Available(EstimatedCost::from_picodollars(u128::MAX)),
    ] {
        let mut combined = summary(
            EstimateTotal::Available(EstimatedCost::from_picodollars(1)),
            1,
        );
        combined.totals.recorded_cost = Some(RecordedCost::from_usd(1.0).unwrap());
        combined.estimates.insert(
            "claude".into(),
            summary(claude_cost, 1)
                .estimates
                .remove(&"codex".into())
                .unwrap(),
        );
        let report = build_usage_report(&combined);
        assert_eq!(report.totals.cost, CostTotal::Overflow);
    }
}

#[test]
fn estimate_only_totals_retain_integer_precision() {
    let cost = EstimatedCost::from_picodollars(u128::MAX);
    let report = build_usage_report(&summary(EstimateTotal::Available(cost), 1));
    assert_eq!(
        report.totals.cost,
        CostTotal::Available {
            amount: CostAmount::Estimated(cost),
            partial: false
        }
    );
}
