use token_tracker::application::{CostAmount, CostTotal, build_usage_report};
use token_tracker::domain::{
    EstimateTotal, EstimateTotals, EstimatedCost, SummaryTotals, UsageSummary,
};

#[test]
fn estimate_only_totals_retain_integer_precision() {
    let cost = EstimatedCost::from_picodollars(u128::MAX);
    let report = build_usage_report(&UsageSummary {
        totals: SummaryTotals {
            estimates: EstimateTotals {
                cost: EstimateTotal::Available(cost),
                imported_event_count: 1,
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
