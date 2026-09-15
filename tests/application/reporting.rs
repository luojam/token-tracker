use token_tracker::application::{CostAmount, CostTotal, build_usage_report};
use token_tracker::domain::{
    EstimateTotal, EstimateTotals, EstimatedCost, RecordedCost, SummaryTotals, UsageSummary,
};

#[test]
fn estimate_only_totals_retain_integer_precision() {
    let cost = EstimatedCost::from_picodollars(u128::MAX);
    let report = build_usage_report(&UsageSummary {
        totals: SummaryTotals {
            estimates: EstimateTotals {
                cost: EstimateTotal::Available(cost),
                estimate_candidate_event_count: 1,
                priced_event_count: 1,
                ..EstimateTotals::default()
            },
            ..SummaryTotals::default()
        },
        ..UsageSummary::default()
    });
    assert_eq!(
        report.totals.cost,
        CostTotal::Available {
            amount: CostAmount::Estimated(cost),
            partial: false
        }
    );
}

#[test]
fn estimate_overflow_invalidates_combined_cost() {
    let report = build_usage_report(&UsageSummary {
        totals: SummaryTotals {
            recorded_cost: Some(RecordedCost::from_usd(1.0).unwrap()),
            estimates: EstimateTotals {
                cost: EstimateTotal::Overflow,
                ..EstimateTotals::default()
            },
            ..SummaryTotals::default()
        },
        ..UsageSummary::default()
    });
    assert_eq!(report.totals.cost, CostTotal::Overflow);
}
