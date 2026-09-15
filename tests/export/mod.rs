mod publishing;

use serde_json::{Value, json};
use token_tracker::domain::export::{
    EXPORT_FORMAT_VERSION, ExportEstimate, ExportSnapshot, UsdAmount,
};
use token_tracker::domain::{EstimatedCost, RecordedCost};

#[test]
fn money_uses_decimal_usd_strings_without_losing_precision() {
    for (picodollars, expected) in [
        (0, "0"),
        (1, "0.000000000001"),
        (120_000_000_000, "0.12"),
        (u128::MAX, "340282366920938463463374607.431768211455"),
    ] {
        let amount = UsdAmount::from(EstimatedCost::from_picodollars(picodollars));
        assert_eq!(serde_json::to_value(&amount).unwrap(), json!(expected));
        assert_eq!(UsdAmount::try_from(expected.to_owned()).unwrap(), amount);
    }

    for value in [0.12, -0.0, f64::from_bits(1), f64::MAX] {
        let amount = UsdAmount::from(RecordedCost::from_usd(value).unwrap());
        let encoded = serde_json::to_string(&amount).unwrap();
        let decoded: UsdAmount = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.as_str().parse::<f64>().unwrap(), value);
    }
    assert_eq!(
        UsdAmount::from(RecordedCost::from_usd(0.12).unwrap()),
        UsdAmount::from(EstimatedCost::from_picodollars(120_000_000_000)),
    );

    for invalid in [json!(0.12), json!("-1"), json!("NaN"), json!("1e-12")] {
        assert!(serde_json::from_value::<UsdAmount>(invalid).is_err());
    }
}

#[test]
fn example_preserves_pricing_context_and_shared_sessions_in_one_event() {
    let expected: Value =
        serde_json::from_str(include_str!("../fixtures/export-example.json")).unwrap();
    let snapshot: ExportSnapshot = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(snapshot.format_version, EXPORT_FORMAT_VERSION);
    assert_eq!(snapshot.events.len(), 1);
    let event = &snapshot.events[0];
    assert_eq!(event.sessions.len(), 2);
    assert!(
        event
            .pricing_context
            .as_ref()
            .unwrap()
            .usage_matches(event.tokens)
    );
    assert_eq!(serde_json::to_value(snapshot).unwrap(), expected);
}

#[test]
fn zero_unavailable_and_recorded_cost_estimates_are_distinct() {
    let mut example: Value =
        serde_json::from_str(include_str!("../fixtures/export-example.json")).unwrap();
    let estimate = &mut example["events"][0]["estimate"];
    estimate["cost_usd"] = json!("0");
    let zero: ExportEstimate = serde_json::from_value(estimate.clone()).unwrap();
    assert!(
        matches!(&zero, ExportEstimate::Available { cost_usd, .. } if cost_usd.as_str() == "0")
    );

    let unavailable = json!({
        "status": "unavailable",
        "reason": "unsupported_model",
        "pricing_version": "openai-api-2026-09-09",
        "pricing_date": "2026-09-09",
        "tier": "standard",
        "tier_evidence": "unknown"
    });
    let decoded: ExportEstimate = serde_json::from_value(unavailable.clone()).unwrap();
    assert!(matches!(decoded, ExportEstimate::Unavailable { .. }));
    assert_eq!(serde_json::to_value(decoded).unwrap(), unavailable);
    assert_eq!(
        serde_json::to_value(ExportEstimate::NotNeeded).unwrap(),
        json!({ "status": "not_needed" }),
    );
    estimate.as_object_mut().unwrap().remove("cost_usd");
    assert!(serde_json::from_value::<ExportEstimate>(estimate.clone()).is_err());
}
