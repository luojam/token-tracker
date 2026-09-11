//! API token estimates exclude regional uplifts, discounts, tool fees, and subscriptions.
//!
//! Rate snapshot sources:
//! - https://developers.openai.com/api/docs/pricing.md (Standard and Fast tables)
//! - https://developers.openai.com/api/docs/models/gpt-6-astra (request threshold)
//! - https://developers.openai.com/api/docs/models/gpt-5.6-sol (threshold and alias)
//! - https://developers.openai.com/api/docs/models/gpt-5.6-terra (threshold and cache writes)
//! - https://developers.openai.com/api/docs/models/gpt-5.6-luna (threshold and cache writes)
//! - https://developers.openai.com/api/docs/models/gpt-5.5 (threshold and snapshot)
//! - https://developers.openai.com/api/docs/models/gpt-5.4-mini (flat rate and snapshot)
//! - https://developers.openai.com/api/docs/guides/prompt-caching (cache-write charges)

use crate::domain::{
    CacheDetail, EstimateUnavailableReason, EstimatedCost, ModelAttribution, PricingContext,
    RequestGranularity, ServiceTier, TierEvidence, TokenCounts, UsageEstimate, UsageEvent,
};

pub const SNAPSHOT_ID: &str = "openai-api-2026-09-09";
pub const RATE_DATE: &str = "2026-09-09";

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

enum ContextRates {
    Flat(TokenRates),
    Banded {
        short_input_limit: u128,
        short: TokenRates,
        long: Option<TokenRates>,
    },
}

impl ContextRates {
    fn for_input(&self, input: u128) -> Result<TokenRates, EstimateUnavailableReason> {
        match self {
            Self::Flat(rates) => Ok(*rates),
            Self::Banded {
                short_input_limit,
                short,
                long,
            } => {
                if input <= *short_input_limit {
                    Ok(*short)
                } else {
                    long.ok_or(EstimateUnavailableReason::UnsupportedContextBand)
                }
            }
        }
    }

    fn supports_aggregate(&self, input: u128) -> bool {
        match self {
            Self::Flat(_) => true,
            Self::Banded {
                short_input_limit, ..
            } => input <= *short_input_limit,
        }
    }
}

struct ModelRates {
    standard: ContextRates,
    fast: ContextRates,
}

const GPT_6_ASTRA: ModelRates = ModelRates {
    standard: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(10_000_000, 1_000_000, 12_500_000, 50_000_000),
        long: Some(TokenRates::new(
            20_000_000, 2_000_000, 25_000_000, 75_000_000,
        )),
    },
    fast: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(20_000_000, 2_000_000, 25_000_000, 100_000_000),
        long: Some(TokenRates::new(
            40_000_000,
            4_000_000,
            50_000_000,
            150_000_000,
        )),
    },
};

const GPT_5_6_SOL: ModelRates = ModelRates {
    standard: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(4_000_000, 400_000, 5_000_000, 20_000_000),
        long: Some(TokenRates::new(8_000_000, 800_000, 10_000_000, 30_000_000)),
    },
    fast: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(8_000_000, 800_000, 10_000_000, 40_000_000),
        long: Some(TokenRates::new(
            16_000_000, 1_600_000, 20_000_000, 60_000_000,
        )),
    },
};

const GPT_5_6_TERRA: ModelRates = ModelRates {
    standard: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(2_000_000, 200_000, 2_500_000, 12_000_000),
        long: Some(TokenRates::new(4_000_000, 400_000, 5_000_000, 18_000_000)),
    },
    fast: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(4_000_000, 400_000, 5_000_000, 24_000_000),
        long: Some(TokenRates::new(8_000_000, 800_000, 10_000_000, 36_000_000)),
    },
};

const GPT_5_6_LUNA: ModelRates = ModelRates {
    standard: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(200_000, 20_000, 250_000, 1_200_000),
        long: Some(TokenRates::new(400_000, 40_000, 500_000, 1_800_000)),
    },
    fast: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(400_000, 40_000, 500_000, 2_400_000),
        long: Some(TokenRates::new(800_000, 80_000, 1_000_000, 3_600_000)),
    },
};

const GPT_5_5: ModelRates = ModelRates {
    standard: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(5_000_000, 500_000, 5_000_000, 30_000_000),
        long: Some(TokenRates::new(
            10_000_000, 1_000_000, 10_000_000, 45_000_000,
        )),
    },
    fast: ContextRates::Banded {
        short_input_limit: 272_000,
        short: TokenRates::new(12_500_000, 1_250_000, 12_500_000, 75_000_000),
        long: None,
    },
};

const GPT_5_4_MINI: ModelRates = ModelRates {
    standard: ContextRates::Flat(TokenRates::new(750_000, 75_000, 750_000, 4_500_000)),
    fast: ContextRates::Flat(TokenRates::new(1_500_000, 150_000, 1_500_000, 9_000_000)),
};

fn schedule(
    attribution: &ModelAttribution,
    tier: &ServiceTier,
) -> Result<&'static ContextRates, EstimateUnavailableReason> {
    if attribution.provider != "openai" {
        return Err(EstimateUnavailableReason::UnsupportedProvider);
    }
    let model = match attribution.model.as_str() {
        "gpt-6-astra" => &GPT_6_ASTRA,
        "gpt-5.6-sol" | "gpt-5.6" => &GPT_5_6_SOL,
        "gpt-5.6-terra" => &GPT_5_6_TERRA,
        "gpt-5.6-luna" => &GPT_5_6_LUNA,
        "gpt-5.5" | "gpt-5.5-2026-04-23" => &GPT_5_5,
        "gpt-5.4-mini" | "gpt-5.4-mini-2026-03-17" => &GPT_5_4_MINI,
        _ => return Err(EstimateUnavailableReason::UnsupportedModel),
    };
    match tier {
        ServiceTier::Standard => Ok(&model.standard),
        ServiceTier::Fast => Ok(&model.fast),
        ServiceTier::Unknown => Err(EstimateUnavailableReason::UnknownTier),
        ServiceTier::Unsupported(_) => Err(EstimateUnavailableReason::UnsupportedTier),
    }
}

/// `request_input` must include ordinary input, cache reads, and cache writes.
pub fn lookup_rates(
    attribution: &ModelAttribution,
    tier: &ServiceTier,
    request_input: u128,
) -> Result<TokenRates, EstimateUnavailableReason> {
    schedule(attribution, tier)?.for_input(request_input)
}

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

/// Unknown tiers use standard rates. The caller must select canonical observations.
pub fn calculate_estimate(
    event: &UsageEvent,
    missing_cache_writes: MissingCacheWritePolicy,
) -> Result<UsageEstimate, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    let context = event
        .pricing_context
        .as_ref()
        .ok_or(Reason::MissingPricingContext)?;
    if context.provider != "openai" {
        return Err(Reason::UnsupportedProvider);
    }
    let attribution = event
        .attribution
        .as_ref()
        .ok_or(Reason::UnknownAttribution)?;
    let (tier, evidence) = estimate_tier(context);
    let schedule = schedule(attribution, &tier)?;
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
    schedule: &ContextRates,
    exact_request: bool,
    missing_cache_writes: MissingCacheWritePolicy,
) -> Result<UsageEstimate, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    let request_input =
        u128::from(tokens.input) + u128::from(tokens.cache_read) + u128::from(tokens.cache_write);
    // If the aggregate fits the short band, every request does too.
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
