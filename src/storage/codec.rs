use super::{SqliteStoreError, parse_notices};
use crate::application::{
    SnapshotCompletion, SourceKey, SourceRevision, SourceState, SuccessfulImport,
};
use crate::domain::{ParentSession, Timestamp, UsageEvent, UsageKind};
use std::{
    ffi::OsString,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

pub(super) fn source_state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceState> {
    let key = SourceKey(row.get(0)?);
    let path = row.get::<_, Option<Vec<u8>>>(1)?.map(decode_path);
    let last_observed_revision = SourceRevision(row.get(2)?);
    let last_imported_revision = row.get::<_, Option<Vec<u8>>>(3)?.map(SourceRevision);
    let last_successful_scan = row
        .get::<_, Option<i64>>(4)?
        .map(Timestamp::from_unix_milliseconds);
    let last_parse_completion = row
        .get::<_, Option<String>>(5)?
        .map(|value| completion_from_str(&value))
        .transpose()
        .map_err(to_sql_conversion_error)?;
    let present = match row.get::<_, i64>(6)? {
        0 => false,
        1 => true,
        _ => return Err(corrupt_sql_value("invalid source presence value")),
    };

    let normalization_version = row
        .get::<_, Option<u32>>(8)?
        .map(|value| {
            NonZeroU32::new(value).ok_or_else(|| corrupt_sql_value("invalid normalization version"))
        })
        .transpose()?;
    let notices =
        parse_notices::decode(&row.get::<_, String>(7)?).map_err(to_sql_conversion_error)?;
    let last_import = match (
        last_imported_revision,
        last_successful_scan,
        last_parse_completion,
        normalization_version,
    ) {
        (None, None, None, None) if notices.is_empty() => None,
        (Some(revision), Some(scanned_at), Some(completion), Some(normalization_version)) => {
            Some(SuccessfulImport {
                revision,
                scanned_at,
                completion,
                normalization_version,
                notices,
            })
        }
        _ => return Err(corrupt_sql_value("incomplete successful import")),
    };
    Ok(SourceState {
        key,
        path,
        last_observed_revision,
        last_import,
        present,
    })
}

pub(super) fn encode_u64(value: u64) -> Result<i64, SqliteStoreError> {
    i64::try_from(value).map_err(|_| SqliteStoreError::ValueOutOfRange("SQLite integer"))
}

pub(super) fn decode_u64(value: i64) -> Result<u64, SqliteStoreError> {
    u64::try_from(value).map_err(|_| SqliteStoreError::CorruptData("negative unsigned integer"))
}

pub(super) fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let normalized: PathBuf = path.components().collect();
    normalized.as_os_str().as_bytes().to_vec()
}

pub(super) fn decode_path(value: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(value))
}

pub(super) fn encode_parent(
    parent: Option<&ParentSession>,
) -> (Option<&'static str>, Option<Vec<u8>>) {
    match parent {
        None => (None, None),
        Some(ParentSession::SessionId(id)) => (Some("session_id"), Some(id.as_bytes().to_vec())),
        Some(ParentSession::SourcePath(path)) => (Some("source_path"), Some(encode_path(path))),
    }
}

pub(super) fn decode_parent(
    kind: Option<String>,
    value: Option<Vec<u8>>,
) -> Result<Option<ParentSession>, SqliteStoreError> {
    match (kind.as_deref(), value) {
        (None, None) => Ok(None),
        (Some("source_path"), Some(value)) => {
            Ok(Some(ParentSession::SourcePath(decode_path(value))))
        }
        (Some("session_id"), Some(value)) => String::from_utf8(value)
            .map(|id| Some(ParentSession::SessionId(id)))
            .map_err(|_| SqliteStoreError::CorruptData("an invalid parent session ID")),
        _ => Err(SqliteStoreError::CorruptData(
            "an invalid parent session reference",
        )),
    }
}

pub(super) fn attribution_parts(event: &UsageEvent) -> (Option<&str>, Option<&str>) {
    match &event.attribution {
        Some(attribution) => (Some(&attribution.provider), Some(&attribution.model)),
        None => (None, None),
    }
}

pub(super) fn usage_kind_to_str(kind: UsageKind) -> &'static str {
    match kind {
        UsageKind::Assistant => "assistant",
        UsageKind::ToolResult => "tool_result",
        UsageKind::Compaction => "compaction",
        UsageKind::BranchSummary => "branch_summary",
        UsageKind::Other => "other",
    }
}

pub(super) fn usage_kind_from_str(value: &str) -> Result<UsageKind, SqliteStoreError> {
    match value {
        "assistant" => Ok(UsageKind::Assistant),
        "tool_result" => Ok(UsageKind::ToolResult),
        "compaction" => Ok(UsageKind::Compaction),
        "branch_summary" => Ok(UsageKind::BranchSummary),
        "other" => Ok(UsageKind::Other),
        _ => Err(SqliteStoreError::CorruptData("an invalid usage kind")),
    }
}

pub(super) fn completion_to_str(completion: SnapshotCompletion) -> &'static str {
    match completion {
        SnapshotCompletion::Complete => "complete",
        SnapshotCompletion::Partial => "partial",
    }
}

pub(super) fn completion_from_str(value: &str) -> Result<SnapshotCompletion, SqliteStoreError> {
    match value {
        "complete" => Ok(SnapshotCompletion::Complete),
        "partial" => Ok(SnapshotCompletion::Partial),
        _ => Err(SqliteStoreError::CorruptData(
            "invalid stored parse completion",
        )),
    }
}

pub(super) fn corrupt_sql_value(message: &'static str) -> rusqlite::Error {
    to_sql_conversion_error(SqliteStoreError::CorruptData(message))
}

pub(super) fn to_sql_conversion_error(error: SqliteStoreError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(error))
}
