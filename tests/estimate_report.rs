use token_tracker::application::{ImportWarning, render_terminal_report};
use token_tracker::core::{
    EstimateBreakdown, EstimateSummary, EstimateTotal, EstimateTotals, EstimatedCost,
    ModelAttribution, RecordedCost, ServiceTier, SummaryBreakdown, SummaryGroup, TokenCounts,
    UsageSummary,
};

fn summary(cost: EstimateTotal, priced: u64) -> UsageSummary {
    UsageSummary {
        estimate: Some(EstimateSummary {
            snapshot_id: "test-snapshot".into(),
            rate_date: "2026-09-07".into(),
            totals: EstimateTotals {
                cost,
                imported_event_count: 1,
                priced_event_count: priced,
                served_response_event_count: priced,
                ..EstimateTotals::default()
            },
            breakdown: vec![],
        }),
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
            &summary(cost, priced),
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
    let estimate = summary.estimate.as_mut().unwrap();
    let attribution = ModelAttribution {
        provider: "custom\nprovider".into(),
        model: "model\t".into(),
    };
    estimate.breakdown.push(EstimateBreakdown {
        attribution: Some(attribution.clone()),
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
    let report = render_terminal_report(&summary, &[]);
    assert!(report.contains("  custom\\nprovider / model\\t "));
    assert!(report.contains("  unavailable\n"));
    assert!(!report.chars().any(|c| c.is_control() && c != '\n'));
}

#[test]
fn model_costs_combine_tiers_and_recorded_costs_without_mixing_providers() {
    let mut summary = summary(
        EstimateTotal::Available(EstimatedCost::from_picodollars(9_000_000_000_000)),
        3,
    );
    summary.totals.recorded_cost = Some(RecordedCost::from_usd(1.0).unwrap());
    summary
        .estimate
        .as_mut()
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
            .estimate
            .as_mut()
            .unwrap()
            .breakdown
            .push(EstimateBreakdown {
                attribution: Some(ModelAttribution {
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
    let report = render_terminal_report(&summary, &[]);
    assert!(
        report.contains("Total cost: $10.000000 (partial)\n"),
        "{report}"
    );
    for (provider, cost) in [("openai", "$6.000000"), ("other", "$4.000000 (partial)")] {
        let line = report
            .lines()
            .find(|line| line.starts_with(&format!("  {provider} / model ")))
            .unwrap();
        assert!(line.ends_with(cost), "{line}");
    }
    assert!(!report.contains("API-equivalent estimate"));
    assert!(!report.contains("Estimates by provider/model/tier"));
}
