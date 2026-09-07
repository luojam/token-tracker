//! Public API token list prices, not historical bills or subscription charges.
//! Excludes regional uplifts, discounts, tool fees, and subscription/credit charges.
//!
//! Verified 2026-09-07:
//! - https://developers.openai.com/api/docs/pricing.md (Standard and Fast tables)
//! - https://developers.openai.com/api/docs/models/gpt-6-astra (request threshold)
//! - https://developers.openai.com/api/docs/models/gpt-5.6-sol (threshold and alias)
//!
//! Sol prices are promotional, available at least through November 21, 2026.
//! Replace this snapshot to revalue stored facts; ordinary runs need no network.

use crate::core::{
    CacheDetail, EstimateUnavailableReason, EstimatedCost, ModelAttribution, RequestGranularity,
    ServiceTier, TierEvidence, TokenCounts, UsageEvent,
};

pub const SNAPSHOT_ID: &str = "openai-api-2026-09-07";
pub const RATE_DATE: &str = "2026-09-07";

/// Integer microdollars per million tokens. Multiplying by tokens gives picodollars.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenRates {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
}

impl TokenRates {
    const fn new(input: u64, cache_read: u64, cache_write: u64, output: u64) -> Self {
        Self {
            input,
            cache_read,
            cache_write,
            output,
        }
    }
}

struct ContextRates {
    short: TokenRates,
    long: TokenRates,
}

struct ModelRates {
    short_input_limit: u128,
    standard: ContextRates,
    fast: ContextRates,
}

const ASTRA: ModelRates = ModelRates {
    short_input_limit: 272_000,
    standard: ContextRates {
        short: TokenRates::new(10_000_000, 1_000_000, 12_500_000, 50_000_000),
        long: TokenRates::new(20_000_000, 2_000_000, 25_000_000, 75_000_000),
    },
    fast: ContextRates {
        short: TokenRates::new(20_000_000, 2_000_000, 25_000_000, 100_000_000),
        long: TokenRates::new(40_000_000, 4_000_000, 50_000_000, 150_000_000),
    },
};

const SOL: ModelRates = ModelRates {
    short_input_limit: 272_000,
    standard: ContextRates {
        short: TokenRates::new(4_000_000, 400_000, 5_000_000, 20_000_000),
        long: TokenRates::new(8_000_000, 800_000, 10_000_000, 30_000_000),
    },
    fast: ContextRates {
        short: TokenRates::new(8_000_000, 800_000, 10_000_000, 40_000_000),
        long: TokenRates::new(16_000_000, 1_600_000, 20_000_000, 60_000_000),
    },
};

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
    let rates = lookup_rates(attribution, &context.tier, request_input)?;
    if context.tier_evidence == TierEvidence::Unknown {
        return Err(Reason::UnknownTier);
    }
    if context.request_granularity != RequestGranularity::ExactSingleRequest {
        return Err(Reason::UnknownRequestGranularity);
    }
    if context.cache_detail != CacheDetail::Complete {
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

/// `request_input` includes ordinary input, cache reads, and cache writes for one
/// exact request, never session totals or configured context capacity. The caller
/// must establish request granularity, cache completeness, and tier evidence.
/// Aliases affect lookup only; the original attribution is not changed.
pub fn lookup_rates(
    attribution: &ModelAttribution,
    tier: &ServiceTier,
    request_input: u128,
) -> Result<TokenRates, EstimateUnavailableReason> {
    if attribution.provider != "openai" {
        return Err(EstimateUnavailableReason::UnsupportedProvider);
    }
    let model = match attribution.model.as_str() {
        "gpt-6-astra" => &ASTRA,
        "gpt-5.6-sol" | "gpt-5.6" => &SOL,
        _ => return Err(EstimateUnavailableReason::UnsupportedModel),
    };
    let rates = match tier {
        ServiceTier::Standard => &model.standard,
        ServiceTier::Fast => &model.fast,
        ServiceTier::Unknown => return Err(EstimateUnavailableReason::UnknownTier),
        ServiceTier::Unsupported(_) => return Err(EstimateUnavailableReason::UnsupportedTier),
    };
    Ok(if request_input <= model.short_input_limit {
        rates.short
    } else {
        rates.long
    })
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
