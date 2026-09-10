use super::TokenCounts;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RawServedValue {
    Missing,
    Null,
    Value(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheCreationTokens {
    pub ephemeral_5m: u64,
    pub ephemeral_1h: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicUsageComponent {
    pub tokens: TokenCounts,
    /// None retains unknown durations, including when the total writes are zero.
    pub cache_creation: Option<CacheCreationTokens>,
}

impl AnthropicUsageComponent {
    fn is_valid(&self) -> bool {
        self.cache_creation.is_none_or(|cache| {
            cache.ephemeral_5m.checked_add(cache.ephemeral_1h) == Some(self.tokens.cache_write)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicIterationKind {
    Message,
    Compaction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicIteration {
    pub kind: AnthropicIterationKind,
    pub usage: AnthropicUsageComponent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AnthropicUsage {
    Response(AnthropicUsageComponent),
    /// Replaces top-level usage; every iteration uses the event's model.
    Iterations(Vec<AnthropicIteration>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnthropicPricingContext {
    pub speed: RawServedValue,
    pub service_tier: RawServedValue,
    pub usage: AnthropicUsage,
}

impl AnthropicPricingContext {
    pub fn usage_matches(&self, tokens: TokenCounts) -> bool {
        match &self.usage {
            AnthropicUsage::Response(component) => {
                component.is_valid() && component.tokens == tokens
            }
            AnthropicUsage::Iterations(iterations) => {
                !iterations.is_empty()
                    && iterations
                        .iter()
                        .all(|iteration| iteration.usage.is_valid())
                    && iterations
                        .iter()
                        .try_fold(TokenCounts::default(), |sum, iteration| {
                            sum.checked_add(iteration.usage.tokens)
                        })
                        == Some(tokens)
            }
        }
    }
}
