//! API-equivalent estimates, separate from recorded costs and subscription charges.
//! Excludes regional uplifts, discounts, tool fees, and subscription/credit charges.

use crate::core::{
    CacheDetail, EstimateUnavailableReason, EstimatedCost, RequestGranularity, TierEvidence,
    TokenCounts, UsageEvent,
};

mod rates;
pub use rates::{RATE_DATE, SNAPSHOT_ID, TokenRates, lookup_rates};

/// Prices one complete observation without changing its facts or recorded cost.
/// Failures prefer context, attribution/rate support, tier evidence, granularity,
/// then cache detail. The caller selects canonical Codex observations.
pub fn calculate_estimate(event: &UsageEvent) -> Result<EstimatedCost, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    let context = event
        .pricing_context
        .as_ref()
        .ok_or(Reason::MissingPricingContext)?;
    let attribution = event
        .attribution
        .as_ref()
        .ok_or(Reason::UnknownAttribution)?;
    let request_input = u128::from(event.tokens.input)
        .checked_add(u128::from(event.tokens.cache_read))
        .and_then(|input| input.checked_add(u128::from(event.tokens.cache_write)))
        .ok_or(Reason::ArithmeticOverflow)?;
    let schedule = rates::schedule(attribution, &context.tier)?;
    if context.tier_evidence == TierEvidence::Unknown {
        return Err(Reason::UnknownTier);
    }
    // An aggregate below the threshold bounds every constituent request below it.
    if context.request_granularity != RequestGranularity::ExactSingleRequest
        && !schedule.supports_aggregate(request_input)
    {
        return Err(Reason::UnknownRequestGranularity);
    }
    let rates = schedule.for_input(request_input)?;
    // Missing cache-write subdivision is harmless only when it costs ordinary input.
    if context.cache_detail != CacheDetail::Complete && rates.cache_write != rates.input {
        return Err(Reason::IncompleteCacheDetail);
    }
    calculate_cost(event.tokens, rates)
}

fn calculate_cost(
    tokens: TokenCounts,
    rates: TokenRates,
) -> Result<EstimatedCost, EstimateUnavailableReason> {
    [
        (tokens.input, rates.input),
        (tokens.cache_read, rates.cache_read),
        (tokens.cache_write, rates.cache_write),
        (tokens.output, rates.output),
    ]
    .into_iter()
    .try_fold(0u128, |total, (count, rate)| {
        u128::from(count)
            .checked_mul(u128::from(rate))
            .and_then(|cost| total.checked_add(cost))
    })
    .map(EstimatedCost::from_picodollars)
    .ok_or(EstimateUnavailableReason::ArithmeticOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_overflow_returns_no_partial_value() {
        // Bundled rates cannot overflow u128 with u64 counts; exercise the guard
        // with extreme rates without exposing a configurable calculator.
        let rates = TokenRates::new(u64::MAX, u64::MAX, 0, 0);
        let mut tokens = TokenCounts {
            input: u64::MAX,
            ..TokenCounts::default()
        };
        assert_eq!(
            calculate_cost(tokens, rates).unwrap().as_picodollars(),
            u128::from(u64::MAX) * u128::from(u64::MAX),
        );
        tokens.cache_read = u64::MAX;
        assert_eq!(
            calculate_cost(tokens, rates),
            Err(EstimateUnavailableReason::ArithmeticOverflow),
        );
    }
}
