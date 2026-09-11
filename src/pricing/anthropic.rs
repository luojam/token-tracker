//! Rate snapshot sources:
//! - https://platform.claude.com/docs/en/about-claude/pricing
//! - https://platform.claude.com/docs/en/models/overview

use crate::domain::{
    CacheWriteTokens, EstimateUnavailableReason, EstimatedCost, ModelAttribution, ServiceTier,
    TierEvidence, TokenCounts, UsageEstimate, UsageEvent,
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
    tier: &ServiceTier,
    tier_evidence: TierEvidence,
    speed: &ServiceTier,
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
    if tier_evidence != TierEvidence::ServedResponse {
        return Err(Reason::UnknownTier);
    }
    match tier {
        ServiceTier::Unknown => return Err(Reason::UnknownTier),
        ServiceTier::Standard => {}
        _ => return Err(Reason::UnsupportedTier),
    }
    match speed {
        ServiceTier::Unknown => Err(Reason::UnknownSpeed),
        ServiceTier::Standard => Ok(standard),
        ServiceTier::Fast if attribution.model == "claude-opus-5" => Ok(OPUS_5_FAST),
        _ => Err(Reason::UnsupportedSpeed),
    }
}

/// Requires served tier evidence and known speed; requested settings cannot substitute.
pub fn calculate_estimate(event: &UsageEvent) -> Result<UsageEstimate, EstimateUnavailableReason> {
    use EstimateUnavailableReason as Reason;

    let context = event
        .pricing_context
        .as_ref()
        .ok_or(Reason::MissingPricingContext)?;
    if context.provider != "anthropic" {
        return Err(Reason::MissingPricingContext);
    }
    let attribution = event
        .attribution
        .as_ref()
        .ok_or(Reason::UnknownAttribution)?;
    let rates = lookup_rates(
        attribution,
        &context.tier,
        context.tier_evidence,
        &context.speed,
    )?;
    if !context.usage_matches(event.tokens) {
        return Err(Reason::InvalidUsageBreakdown);
    }
    let cost = price_tokens(event.tokens, context.cache_writes.as_deref(), rates)?;
    Ok(UsageEstimate {
        cost,
        assumed_cache_writes_as_input: false,
    })
}

fn price_tokens(
    tokens: TokenCounts,
    durations: Option<&[CacheWriteTokens]>,
    rates: TokenRates,
) -> Result<EstimatedCost, EstimateUnavailableReason> {
    let mut cost = [
        (tokens.input, rates.input),
        (tokens.output, rates.output),
        (tokens.cache_read, rates.cache_read),
    ]
    .into_iter()
    .try_fold(0u128, |total, (count, rate)| {
        u128::from(count)
            .checked_mul(u128::from(rate))
            .and_then(|cost| total.checked_add(cost))
    })
    .ok_or(EstimateUnavailableReason::ArithmeticOverflow)?;
    match durations {
        Some(durations) => {
            for duration in durations {
                let rate = match duration.duration_seconds {
                    300 => rates.cache_write_5m,
                    3600 => rates.cache_write_1h,
                    _ => return Err(EstimateUnavailableReason::IncompleteCacheDetail),
                };
                cost = cost
                    .checked_add(u128::from(duration.tokens) * u128::from(rate))
                    .ok_or(EstimateUnavailableReason::ArithmeticOverflow)?;
            }
        }
        None if tokens.cache_write > 0 => {
            return Err(EstimateUnavailableReason::IncompleteCacheDetail);
        }
        None => {}
    }
    Ok(EstimatedCost::from_picodollars(cost))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_overflow_returns_no_partial_value() {
        let rates = TokenRates {
            input: u64::MAX,
            output: u64::MAX,
            ..OPUS_5
        };
        let tokens = TokenCounts {
            input: u64::MAX,
            ..TokenCounts::default()
        };
        assert_eq!(
            price_tokens(tokens, None, rates).unwrap().as_picodollars(),
            u128::from(u64::MAX) * u128::from(u64::MAX)
        );
        assert_eq!(
            price_tokens(
                TokenCounts {
                    output: u64::MAX,
                    ..tokens
                },
                None,
                rates
            ),
            Err(EstimateUnavailableReason::ArithmeticOverflow)
        );
    }
}
