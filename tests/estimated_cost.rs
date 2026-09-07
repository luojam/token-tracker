use token_tracker::core::{EstimateTotal, EstimatedCost};

#[test]
fn displays_six_decimal_usd_without_losing_zero_or_rounding_overflow() {
    for (picodollars, expected) in [
        (0, "$0.000000"),
        (1, "<$0.000001"),
        (499_999, "<$0.000001"),
        (500_000, "<$0.000001"),
        (999_999, "<$0.000001"),
        (1_000_000, "$0.000001"),
        (1_499_999, "$0.000001"),
        (1_500_000, "$0.000002"),
        (999_999_499_999, "$0.999999"),
        (999_999_500_000, "$1.000000"),
        (u128::MAX, "$340282366920938463463374607.431768"),
    ] {
        assert_eq!(
            EstimatedCost::from_picodollars(picodollars).to_string(),
            expected,
            "{picodollars} picodollars",
        );
    }
}

#[test]
fn addition_preserves_precision_and_distinguishes_zero_unavailable_and_overflow() {
    let tiny = EstimatedCost::from_picodollars(750_000);
    let sum = tiny.checked_add(tiny).unwrap();
    assert_eq!(sum.as_picodollars(), 1_500_000);
    assert_eq!(sum.to_string(), "$0.000002");

    let zero = EstimatedCost::from_picodollars(0);
    let max = EstimatedCost::from_picodollars(u128::MAX);
    let one = EstimatedCost::from_picodollars(1);
    assert_eq!(zero.checked_add(zero), Some(zero));
    assert_eq!(max.checked_add(zero), Some(max));
    assert_eq!(
        EstimatedCost::from_picodollars(u128::MAX - 1).checked_add(one),
        Some(max),
    );
    assert_eq!(max.checked_add(one), None);
    assert_eq!(one.checked_add(max), None);
    assert_ne!(EstimateTotal::Unavailable, EstimateTotal::Available(zero));
    assert_ne!(EstimateTotal::Overflow, EstimateTotal::Available(zero));
    assert_ne!(EstimateTotal::Overflow, EstimateTotal::Unavailable);
}
