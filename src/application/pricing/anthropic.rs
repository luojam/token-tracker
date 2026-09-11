//! Global API token prices, verified 2026-09-11:
//! - https://platform.claude.com/docs/en/about-claude/pricing
//! - https://platform.claude.com/docs/en/models/overview
//!
//! Fast cache multipliers apply to fast input prices; these models have flat rates.

use crate::core::{
    AnthropicUsage, AnthropicUsageComponent, CacheCreationTokens, EstimateUnavailableReason,
    EstimatedCost, ModelAttribution, RawServedValue, UsageEstimate, UsageEvent,
};

pub const SNAPSHOT_ID: &str = "anthropic-api-2026-09-11";
pub const RATE_DATE: &str = "2026-09-11";

/// Integer microdollars per million tokens; count times rate gives picodollars.
#[derive(Clone, Copy)]
struct TokenRates {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write_5m: u64,
    cache_write_1h: u64,
}

const FABLE_5: TokenRates = TokenRates {
    input: 10_000_000,
    output: 50_000_000,
    cache_read: 1_000_000,
    cache_write_5m: 12_500_000,
    cache_write_1h: 20_000_000,
};
const FABLE_5_1: TokenRates = TokenRates {
    cache_read: 250_000,
    ..FABLE_5
};
const OPUS_5: TokenRates = TokenRates {
    input: 5_000_000,
    output: 25_000_000,
    cache_read: 500_000,
    cache_write_5m: 6_250_000,
    cache_write_1h: 10_000_000,
};
const OPUS_5_FAST: TokenRates = TokenRates {
    input: 10_000_000,
    output: 50_000_000,
    cache_read: 1_000_000,
    cache_write_5m: 12_500_000,
    cache_write_1h: 20_000_000,
};
const SONNET_5: TokenRates = TokenRates {
    input: 2_000_000,
    output: 10_000_000,
    cache_read: 200_000,
    cache_write_5m: 2_500_000,
    cache_write_1h: 4_000_000,
};
const HAIKU_4_5: TokenRates = TokenRates {
    input: 1_000_000,
    output: 5_000_000,
    cache_read: 100_000,
    cache_write_5m: 1_250_000,
    cache_write_1h: 2_000_000,
};

fn lookup_rates(
    attribution: &ModelAttribution,
    tier: &RawServedValue,
    speed: &RawServedValue,
) -> Result<TokenRates, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    if attribution.provider != "anthropic" {
        return Err(Reason::UnsupportedProvider);
    }
    let standard = match attribution.model.as_str() {
        "claude-fable-5" => FABLE_5,
        "claude-fable-5-1" => FABLE_5_1,
        "claude-opus-5" => OPUS_5,
        "claude-sonnet-5" => SONNET_5,
        "claude-haiku-4-5-20251001" => HAIKU_4_5,
        _ => return Err(Reason::UnsupportedModel),
    };
    match tier {
        RawServedValue::Missing | RawServedValue::Null => return Err(Reason::UnknownTier),
        RawServedValue::Value(value) if value == "standard" => {}
        RawServedValue::Value(_) => return Err(Reason::UnsupportedTier),
    }
    match speed {
        RawServedValue::Missing | RawServedValue::Null => Err(Reason::UnknownSpeed),
        RawServedValue::Value(value) if value == "standard" => Ok(standard),
        RawServedValue::Value(value) if value == "fast" && attribution.model == "claude-opus-5" => {
            Ok(OPUS_5_FAST)
        }
        RawServedValue::Value(_) => Err(Reason::UnsupportedSpeed),
    }
}

/// Prices a canonical Claude observation using only its Anthropic served facts.
/// Failures prefer context, attribution/model, tier, speed, usage validity, then
/// cache duration. Missing evidence never falls back to a requested setting.
pub fn calculate_estimate(event: &UsageEvent) -> Result<UsageEstimate, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    let context = event
        .pricing_context
        .as_ref()
        .ok_or(Reason::MissingPricingContext)?;
    let facts = context
        .anthropic
        .as_ref()
        .ok_or(Reason::MissingPricingContext)?;
    let attribution = event
        .attribution
        .as_ref()
        .ok_or(Reason::UnknownAttribution)?;
    let rates = lookup_rates(attribution, &facts.service_tier, &facts.speed)?;
    if !context.usage_matches(event.tokens) {
        return Err(Reason::InvalidUsageBreakdown);
    }
    let cost = match &facts.usage {
        AnthropicUsage::Response(component) => price_component(component, rates)?,
        AnthropicUsage::Iterations(iterations) => {
            iterations
                .iter()
                .try_fold(EstimatedCost::default(), |total, iteration| {
                    total
                        .checked_add(price_component(&iteration.usage, rates)?)
                        .ok_or(Reason::ArithmeticOverflow)
                })?
        }
    };
    Ok(UsageEstimate {
        cost,
        assumed_cache_writes_as_input: false,
    })
}

fn price_component(
    component: &AnthropicUsageComponent,
    rates: TokenRates,
) -> Result<EstimatedCost, EstimateUnavailableReason> {
    let cache = match component.cache_creation {
        Some(cache) => cache,
        None if component.tokens.cache_write == 0 => CacheCreationTokens {
            ephemeral_5m: 0,
            ephemeral_1h: 0,
        },
        None => return Err(EstimateUnavailableReason::IncompleteCacheDetail),
    };
    [
        (component.tokens.input, rates.input),
        (component.tokens.output, rates.output),
        (component.tokens.cache_read, rates.cache_read),
        (cache.ephemeral_5m, rates.cache_write_5m),
        (cache.ephemeral_1h, rates.cache_write_1h),
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
    use crate::core::TokenCounts;

    #[test]
    fn cost_overflow_returns_no_partial_value() {
        // Bundled rates cannot overflow u128 with u64 counts.
        let rates = TokenRates {
            input: u64::MAX,
            output: u64::MAX,
            ..OPUS_5
        };
        let mut component = AnthropicUsageComponent {
            tokens: TokenCounts {
                input: u64::MAX,
                ..TokenCounts::default()
            },
            cache_creation: None,
        };
        assert_eq!(
            price_component(&component, rates).unwrap().as_picodollars(),
            u128::from(u64::MAX) * u128::from(u64::MAX),
        );
        component.tokens.output = u64::MAX;
        assert_eq!(
            price_component(&component, rates),
            Err(EstimateUnavailableReason::ArithmeticOverflow)
        );
    }
}
