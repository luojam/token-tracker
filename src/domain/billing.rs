use super::{CacheDetail, RequestGranularity, ServiceTier, TierEvidence, TokenCounts};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheWriteTokens {
    pub duration_seconds: u32,
    pub tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingContext {
    pub provider: String,
    pub tier: ServiceTier,
    pub tier_evidence: TierEvidence,
    pub speed: ServiceTier,
    pub request_granularity: RequestGranularity,
    pub cache_detail: CacheDetail,
    pub request_usage: Option<Vec<TokenCounts>>,
    pub cache_writes: Option<Vec<CacheWriteTokens>>,
}

impl PricingContext {
    pub fn usage_matches(&self, tokens: TokenCounts) -> bool {
        self.request_usage_matches(tokens)
            && self.cache_writes.as_ref().is_none_or(|writes| {
                writes
                    .iter()
                    .try_fold(0u64, |total, write| total.checked_add(write.tokens))
                    == Some(tokens.cache_write)
            })
    }

    pub fn request_usage_matches(&self, tokens: TokenCounts) -> bool {
        self.request_usage.as_ref().is_none_or(|requests| {
            !requests.is_empty()
                && requests
                    .iter()
                    .try_fold(TokenCounts::default(), |total, request| {
                        total.checked_add(*request)
                    })
                    == Some(tokens)
        })
    }
}
