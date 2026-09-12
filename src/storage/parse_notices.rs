use super::SqliteStoreError;
use crate::application::{ParseNotice, validate_notices};

pub(super) fn encode(notices: &[ParseNotice]) -> Result<String, SqliteStoreError> {
    serde_json::to_string(notices).map_err(SqliteStoreError::Serialization)
}

pub(super) fn decode(value: &str) -> Result<Vec<ParseNotice>, SqliteStoreError> {
    let notices: Vec<ParseNotice> = serde_json::from_str(value)
        .map_err(|_| SqliteStoreError::CorruptData("invalid parse notices"))?;
    if validate_notices(&notices).is_err() {
        return Err(SqliteStoreError::CorruptData(
            "duplicate parse notice codes",
        ));
    }
    Ok(notices)
}
