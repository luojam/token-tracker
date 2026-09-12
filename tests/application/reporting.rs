use token_tracker::application::{CostAmount, CostTotal, ImportWarning, build_usage_report};
use token_tracker::cli::render_terminal_report;
use token_tracker::domain::{
    EstimateTotal, EstimateTotals, EstimatedCost, ModelAttribution, ServiceTier, SummaryBreakdown,
    SummaryGroup, SummaryTotals, TokenCounts, UsageSummary,
};

fn summary(cost: EstimateTotal, priced: u64) -> UsageSummary {
    UsageSummary {
        totals: SummaryTotals {
            estimates: EstimateTotals {
                cost,
                imported_event_count: 1,
                priced_event_count: priced,
                served_response_event_count: priced,
                ..EstimateTotals::default()
            },
            ..SummaryTotals::default()
        },
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
    summary
        .totals
        .estimates
        .tier_event_counts
        .insert(ServiceTier::Unsupported("priority\r\u{1b}".into()), 1);
    summary
        .totals
        .estimates
        .rate_snapshots
        .insert("snapshot\n".into(), "date\t".into());
    let attribution = ModelAttribution {
        provider: "custom\nprovider".into(),
        model: "model\t".into(),
    };
    summary.breakdown.push(SummaryBreakdown {
        agent: "codex".into(),
        group: SummaryGroup::ProviderModel(attribution),
        tokens: TokenCounts::default(),
        recorded_cost: None,
        estimates: summary.totals.estimates.clone(),
        unique_usage_event_count: 1,
    });
    let report = render_terminal_report(&build_usage_report(&summary), &[]);
    assert!(report.contains("  custom\\nprovider / model\\t "));
    assert!(report.contains("  unavailable\n"));
    assert!(!report.chars().any(|c| c.is_control() && c != '\n'));
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
