use super::{SqliteStoreError, parse_notices};
use crate::application::{DiscoveryReport, FileRevision, ParseCompletion, SourceState};
use crate::domain::{ParentSession, Timestamp, UsageEvent, UsageKind};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) fn source_state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceState> {
    let path = decode_path(row.get(0)?);
    let last_observed_revision = revision_from_columns(row, 1, 2, 3)?
        .ok_or_else(|| corrupt_sql_value("last observed revision is incomplete"))?;
    let last_imported_revision = revision_from_columns(row, 4, 5, 6)?;
    let last_successful_scan = row
        .get::<_, Option<i64>>(7)?
        .map(Timestamp::from_unix_milliseconds);
    let last_parse_completion = row
        .get::<_, Option<String>>(8)?
        .map(|value| completion_from_str(&value))
        .transpose()
        .map_err(to_sql_conversion_error)?;
    let present = match row.get::<_, i64>(9)? {
        0 => false,
        1 => true,
        _ => return Err(corrupt_sql_value("invalid source presence value")),
    };

    Ok(SourceState {
        path,
        last_observed_revision,
        last_imported_revision,
        last_successful_scan,
        last_parse_completion,
        normalization_version: row.get(11)?,
        notices: parse_notices::decode(&row.get::<_, String>(10)?)
            .map_err(to_sql_conversion_error)?,
        present,
    })
}

pub(super) fn revision_from_columns(
    row: &rusqlite::Row<'_>,
    size_column: usize,
    seconds_column: usize,
    nanos_column: usize,
) -> rusqlite::Result<Option<FileRevision>> {
    let size = row.get::<_, Option<i64>>(size_column)?;
    let seconds = row.get::<_, Option<i64>>(seconds_column)?;
    let nanos = row.get::<_, Option<u32>>(nanos_column)?;
    match (size, seconds, nanos) {
        (None, None, None) => Ok(None),
        (Some(size), Some(seconds), Some(nanos)) => Ok(Some(FileRevision {
            size: decode_u64(size).map_err(to_sql_conversion_error)?,
            modified_at: system_time_from_parts(seconds, nanos).map_err(to_sql_conversion_error)?,
        })),
        _ => Err(corrupt_sql_value("incomplete file revision")),
    }
}

pub(super) fn discovery_covers(path: &Path, report: &DiscoveryReport) -> bool {
    report
        .coverage
        .inspected_roots
        .iter()
        .any(|root| path.starts_with(root))
        && !report
            .coverage
            .inaccessible_paths
            .iter()
            .any(|inaccessible| path.starts_with(inaccessible))
}

pub(super) fn system_time_to_parts(value: SystemTime) -> Result<(i64, u32), SqliteStoreError> {
    match value.duration_since(UNIX_EPOCH) {
        Ok(duration) => Ok((
            i64::try_from(duration.as_secs())
                .map_err(|_| SqliteStoreError::ValueOutOfRange("file modification time"))?,
            duration.subsec_nanos(),
        )),
        Err(error) => {
            let duration = error.duration();
            let seconds = i64::try_from(duration.as_secs())
                .map_err(|_| SqliteStoreError::ValueOutOfRange("file modification time"))?;
            if duration.subsec_nanos() == 0 {
                Ok((-seconds, 0))
            } else {
                Ok((
                    seconds
                        .checked_add(1)
                        .and_then(|seconds| seconds.checked_neg())
                        .ok_or(SqliteStoreError::ValueOutOfRange("file modification time"))?,
                    1_000_000_000 - duration.subsec_nanos(),
                ))
            }
        }
    }
}

pub(super) fn system_time_from_parts(
    seconds: i64,
    nanos: u32,
) -> Result<SystemTime, SqliteStoreError> {
    if nanos >= 1_000_000_000 {
        return Err(SqliteStoreError::CorruptData(
            "file modification nanoseconds are out of range",
        ));
    }
    if seconds >= 0 {
        return UNIX_EPOCH
            .checked_add(Duration::new(seconds as u64, nanos))
            .ok_or(SqliteStoreError::ValueOutOfRange("file modification time"));
    }

    let seconds_magnitude = seconds.unsigned_abs();
    let duration = if nanos == 0 {
        Duration::new(seconds_magnitude, 0)
    } else {
        Duration::new(seconds_magnitude - 1, 1_000_000_000 - nanos)
    };
    UNIX_EPOCH
        .checked_sub(duration)
        .ok_or(SqliteStoreError::ValueOutOfRange("file modification time"))
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

pub(super) fn completion_to_str(completion: ParseCompletion) -> &'static str {
    match completion {
        ParseCompletion::Complete => "complete",
        ParseCompletion::IncompleteFinalLine => "incomplete_final_line",
    }
}

pub(super) fn completion_from_str(value: &str) -> Result<ParseCompletion, SqliteStoreError> {
    match value {
        "complete" => Ok(ParseCompletion::Complete),
        "incomplete_final_line" => Ok(ParseCompletion::IncompleteFinalLine),
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
