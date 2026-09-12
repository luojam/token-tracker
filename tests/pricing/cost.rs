use token_tracker::domain::EstimatedCost;

#[test]
fn displays_six_decimal_usd_without_losing_zero_or_rounding_overflow() {
    for (picodollars, expected) in [
        (0, "$0.000000"),
        (1, "<$0.000001"),
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
fn addition_preserves_precision_and_rejects_overflow() {
    let tiny = EstimatedCost::from_picodollars(750_000);
    let sum = tiny.checked_add(tiny).unwrap();
    assert_eq!(sum.as_picodollars(), 1_500_000);
    assert_eq!(sum.to_string(), "$0.000002");

    assert_eq!(
        EstimatedCost::from_picodollars(u128::MAX).checked_add(tiny),
        None
    );
}
