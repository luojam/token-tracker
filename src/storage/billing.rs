use super::SqliteStoreError;
use crate::domain::{PricingContext, TokenCounts};

pub(super) fn encode(context: Option<&PricingContext>) -> Result<Option<String>, SqliteStoreError> {
    context
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| SqliteStoreError::InvalidImport("invalid billing inputs"))
}

pub(super) fn decode(
    facts: Option<String>,
    tokens: TokenCounts,
) -> Result<Option<PricingContext>, SqliteStoreError> {
    facts
        .map(|facts| {
            let context: PricingContext = serde_json::from_str(&facts)
                .map_err(|_| SqliteStoreError::CorruptData("invalid billing inputs"))?;
            if !context.usage_matches(tokens) {
                return Err(SqliteStoreError::CorruptData(
                    "billing inputs do not match usage",
                ));
            }
            Ok(context)
        })
        .transpose()
}
