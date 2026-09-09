//! API-equivalent estimates, separate from recorded costs and subscription charges.
//! Excludes regional uplifts, discounts, tool fees, and subscription/credit charges.

use crate::core::{
    CacheDetail, EstimateUnavailableReason, EstimatedCost, PricingContext, RequestGranularity,
    ServiceTier, TierEvidence, TokenCounts, UsageEstimate, UsageEvent,
};

mod rates;
pub use rates::{RATE_DATE, SNAPSHOT_ID, TokenRates, lookup_rates};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissingCacheWritePolicy {
    Reject,
    TreatAsInput,
}

pub(super) fn estimate_tier(context: &PricingContext) -> (ServiceTier, TierEvidence) {
    match context.tier {
        ServiceTier::Unknown => (ServiceTier::Standard, TierEvidence::Unknown),
        _ => (context.tier.clone(), context.tier_evidence),
    }
}

/// Prices one complete observation without changing its facts or recorded cost.
/// Unknown tiers use standard rates; the caller chooses how to handle missing writes.
/// Failures prefer context, attribution/rate support, tier evidence, granularity,
/// then cache detail. The caller selects canonical Codex observations.
pub fn calculate_estimate(
    event: &UsageEvent,
    missing_cache_writes: MissingCacheWritePolicy,
) -> Result<UsageEstimate, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    let context = event
        .pricing_context
        .as_ref()
        .ok_or(Reason::MissingPricingContext)?;
    let attribution = event
        .attribution
        .as_ref()
        .ok_or(Reason::UnknownAttribution)?;
    let (tier, evidence) = estimate_tier(context);
    let schedule = rates::schedule(attribution, &tier)?;
    if context.tier != ServiceTier::Unknown && evidence == TierEvidence::Unknown {
        return Err(Reason::UnknownTier);
    }
    if !context.request_usage_matches(event.tokens) {
        return Err(Reason::UnknownRequestGranularity);
    }
    if let Some(requests) = &context.request_usage {
        return requests
            .iter()
            .try_fold(UsageEstimate::default(), |total, tokens| {
                let estimate =
                    price_tokens(*tokens, context, schedule, true, missing_cache_writes)?;
                Ok(UsageEstimate {
                    cost: total
                        .cost
                        .checked_add(estimate.cost)
                        .ok_or(Reason::ArithmeticOverflow)?,
                    assumed_cache_writes_as_input: total.assumed_cache_writes_as_input
                        || estimate.assumed_cache_writes_as_input,
                })
            });
    }
    price_tokens(
        event.tokens,
        context,
        schedule,
        context.request_granularity == RequestGranularity::ExactSingleRequest,
        missing_cache_writes,
    )
}

fn price_tokens(
    tokens: TokenCounts,
    context: &PricingContext,
    schedule: &rates::ContextRates,
    exact_request: bool,
    missing_cache_writes: MissingCacheWritePolicy,
) -> Result<UsageEstimate, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    let request_input =
        u128::from(tokens.input) + u128::from(tokens.cache_read) + u128::from(tokens.cache_write);
    // An aggregate below the threshold bounds every constituent request below it.
    if !exact_request && !schedule.supports_aggregate(request_input) {
        return Err(Reason::UnknownRequestGranularity);
    }
    let rates = schedule.for_input(request_input)?;
    if context.cache_detail != CacheDetail::Complete
        && rates.cache_write != rates.input
        && missing_cache_writes == MissingCacheWritePolicy::Reject
    {
        return Err(Reason::IncompleteCacheDetail);
    }
    Ok(UsageEstimate {
        cost: calculate_cost(tokens, rates)?,
        assumed_cache_writes_as_input: context.cache_detail == CacheDetail::Incomplete
            && rates.cache_write != rates.input
            && tokens.input > 0,
    })
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
