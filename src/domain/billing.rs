use super::{CacheDetail, ServiceTier, TierEvidence, TokenCounts};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheWriteTokens {
    pub duration_seconds: u32,
    pub tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceSpeed {
    Standard,
    Fast,
    Unknown,
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestBreakdown {
    SingleRequest,
    KnownRequests(KnownRequests),
    AggregateOrUnknown,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "Vec<TokenCounts>")]
pub struct KnownRequests(Vec<TokenCounts>);

impl TryFrom<Vec<TokenCounts>> for KnownRequests {
    type Error = &'static str;

    fn try_from(requests: Vec<TokenCounts>) -> Result<Self, Self::Error> {
        Self::from_vec(requests).ok_or("request breakdown must not be empty")
    }
}

impl KnownRequests {
    pub fn new(first: TokenCounts) -> Self {
        Self(vec![first])
    }

    pub fn from_vec(requests: Vec<TokenCounts>) -> Option<Self> {
        (!requests.is_empty()).then_some(Self(requests))
    }

    pub fn push(&mut self, request: TokenCounts) {
        self.0.push(request);
    }

    pub fn as_slice(&self) -> &[TokenCounts] {
        &self.0
    }
}

impl RequestBreakdown {
    pub fn usage_matches(&self, tokens: TokenCounts) -> bool {
        match self {
            Self::KnownRequests(requests) => {
                requests
                    .as_slice()
                    .iter()
                    .try_fold(TokenCounts::default(), |total, request| {
                        total.checked_add(*request)
                    })
                    == Some(tokens)
            }
            Self::SingleRequest | Self::AggregateOrUnknown => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiBilling {
    pub tier: ServiceTier,
    pub tier_evidence: TierEvidence,
    pub requests: RequestBreakdown,
    pub cache_detail: CacheDetail,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicBilling {
    pub tier: ServiceTier,
    pub tier_evidence: TierEvidence,
    pub speed: ServiceSpeed,
    pub requests: RequestBreakdown,
    pub cache_writes: Option<Vec<CacheWriteTokens>>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingContext {
    OpenAi(OpenAiBilling),
    Anthropic(AnthropicBilling),
}

impl PricingContext {
    pub fn usage_matches(&self, tokens: TokenCounts) -> bool {
        match self {
            Self::OpenAi(context) => context.requests.usage_matches(tokens),
            Self::Anthropic(context) => {
                context.requests.usage_matches(tokens)
                    && cache_writes_match(context.cache_writes.as_deref(), tokens)
            }
        }
    }
}

fn cache_writes_match(writes: Option<&[CacheWriteTokens]>, tokens: TokenCounts) -> bool {
    writes.is_none_or(|writes| {
        writes
            .iter()
            .try_fold(0u64, |total, write| total.checked_add(write.tokens))
            == Some(tokens.cache_write)
    })
}
