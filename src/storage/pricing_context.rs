use super::SqliteStoreError;
use crate::domain::{PricingContext, TokenCounts};

pub(super) fn encode(context: Option<&PricingContext>) -> Result<Option<String>, SqliteStoreError> {
    context
        .map(serde_json::to_string)
        .transpose()
        .map_err(SqliteStoreError::Serialization)
}

pub(super) fn decode(
    encoded: Option<String>,
    tokens: TokenCounts,
) -> Result<Option<PricingContext>, SqliteStoreError> {
    encoded
        .map(|encoded| {
            let context: PricingContext = serde_json::from_str(&encoded)
                .map_err(|_| SqliteStoreError::CorruptData("invalid pricing context"))?;
            if !context.usage_matches(tokens) {
                return Err(SqliteStoreError::CorruptData(
                    "pricing context does not match usage",
                ));
            }
            Ok(context)
        })
        .transpose()
}
