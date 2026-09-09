//! Public API token list prices, verified 2026-09-09:
//! - https://developers.openai.com/api/docs/pricing.md (Standard and Fast tables)
//! - https://developers.openai.com/api/docs/models/gpt-6-astra (request threshold)
//! - https://developers.openai.com/api/docs/models/gpt-5.6-sol (threshold and alias)
//! - https://developers.openai.com/api/docs/models/gpt-5.6-terra (threshold and cache writes)
//! - https://developers.openai.com/api/docs/models/gpt-5.6-luna (threshold and cache writes)
//! - https://developers.openai.com/api/docs/models/gpt-5.5 (threshold and snapshot)
//! - https://developers.openai.com/api/docs/models/gpt-5.4-mini (flat rate and snapshot)
//! - https://developers.openai.com/api/docs/guides/prompt-caching (cache-write charges)
//!
//! GPT-5.6 Sol prices are promotional, available at least through November 21, 2026.
//! Earlier models charge cache writes as ordinary input, with no extra fee.
//! No published API rate or model mapping was found for codex-auto-review.

use crate::core::{EstimateUnavailableReason, ModelAttribution, ServiceTier};

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
    pub(super) const fn new(input: u64, cache_read: u64, cache_write: u64, output: u64) -> Self {
        Self {
            input,
            cache_read,
            cache_write,
            output,
        }
    }
}

pub(super) enum ContextRates {
    Flat(TokenRates),
    Banded {
        short_input_limit: u128,
        short: TokenRates,
        long: Option<TokenRates>,
    },
}

impl ContextRates {
    pub(super) fn for_input(&self, input: u128) -> Result<TokenRates, EstimateUnavailableReason> {
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

    pub(super) fn supports_aggregate(&self, input: u128) -> bool {
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
        // No published GPT-5.5 Fast rate above 272K input tokens.
        long: None,
    },
};

const GPT_5_4_MINI: ModelRates = ModelRates {
    standard: ContextRates::Flat(TokenRates::new(750_000, 75_000, 750_000, 4_500_000)),
    fast: ContextRates::Flat(TokenRates::new(1_500_000, 150_000, 1_500_000, 9_000_000)),
};

pub(super) fn schedule(
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

/// Looks up one request's rates, including all input and cache tokens in its band.
/// The caller establishes granularity, cache completeness, and tier evidence.
pub fn lookup_rates(
    attribution: &ModelAttribution,
    tier: &ServiceTier,
    request_input: u128,
) -> Result<TokenRates, EstimateUnavailableReason> {
    schedule(attribution, tier)?.for_input(request_input)
}
