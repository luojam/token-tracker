use std::collections::HashSet;

use super::SqliteStoreError;
use crate::application::ParseNotice;

pub(super) fn encode(notices: &[ParseNotice]) -> Result<String, SqliteStoreError> {
    if !unique_codes(notices) {
        return Err(SqliteStoreError::InvalidImport(
            "duplicate parse notice codes",
        ));
    }
    serde_json::to_string(notices)
        .map_err(|_| SqliteStoreError::InvalidImport("invalid parse notices"))
}

pub(super) fn decode(value: &str) -> Result<Vec<ParseNotice>, SqliteStoreError> {
    let notices: Vec<ParseNotice> = serde_json::from_str(value)
        .map_err(|_| SqliteStoreError::CorruptData("invalid parse notices"))?;
    if !unique_codes(&notices) {
        return Err(SqliteStoreError::CorruptData(
            "duplicate parse notice codes",
        ));
    }
    Ok(notices)
}

fn unique_codes(notices: &[ParseNotice]) -> bool {
    let mut codes = HashSet::new();
    notices.iter().all(|notice| codes.insert(notice.code))
}
