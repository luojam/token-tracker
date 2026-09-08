use token_tracker::application::{ImportWarning, render_terminal_report};
use token_tracker::core::{
    EstimateBreakdown, EstimateSummary, EstimateTotal, EstimateTotals, EstimatedCost,
    ModelAttribution, ServiceTier, UsageSummary,
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
        assert!(report.contains(&format!("Estimated total: {label}\n")));
        assert!(report.contains(&format!(
            "Coverage: {priced} / 1 imported canonical Codex events priced\n"
        )));
        assert!(!report.contains("(partial)"));
        assert!(!report.contains("Requested settings do not confirm"));
        assert!(report.ends_with("Warnings (1):\n- could not parse\n"));
    }
}

#[test]
fn estimate_labels_escape_source_control_characters() {
    let mut summary = summary(EstimateTotal::Unavailable, 0);
    let estimate = summary.estimate.as_mut().unwrap();
    estimate.breakdown.push(EstimateBreakdown {
        attribution: Some(ModelAttribution {
            provider: "custom\nprovider".into(),
            model: "model\t".into(),
        }),
        tier: ServiceTier::Unsupported("priority\r\u{1b}".into()),
        totals: estimate.totals.clone(),
    });
    let report = render_terminal_report(&summary, &[]);
    assert!(report.contains(
        "- custom\\nprovider / model\\t / unsupported (priority\\r\\u{1b}): unavailable"
    ));
    assert!(!report.chars().any(|c| c.is_control() && c != '\n'));
}
