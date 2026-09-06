//! SQLite persistence for imported session metadata and usage observations.

mod importing;
mod migrations;
mod reading;
#[cfg(test)]
mod tests;

use importing::{
    import_is_stale, insert_observation, update_observation, upsert_imported_source,
    upsert_source_session, validate_import,
};
use migrations::migrate;
use reading::{load_stored_observations, load_stored_sessions};

use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, TransactionBehavior, params};

use crate::application::{
    CommitImportOutcome, DiscoveryReport, FileRevision, ImportStats, ParseCompletion,
    SessionImport, SourceState, UsageReadStore, UsageSnapshot, UsageStore,
};
use crate::core::{AgentId, ParentSession, Timestamp, UsageEvent, UsageKind};

const APPLICATION_DIRECTORY: &str = "token-tracker";
const DATABASE_FILENAME: &str = "usage.db";

/// Resolves the database location documented for the command-line application.
pub fn default_database_path() -> Result<PathBuf, SqliteStoreError> {
    default_database_path_from(
        env::var_os("XDG_DATA_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn default_database_path_from(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, SqliteStoreError> {
    let data_home = match absolute_environment_path(xdg_data_home) {
        Some(path) => path,
        None => absolute_environment_path(home)
            .ok_or(SqliteStoreError::HomeDirectoryUnavailable)?
            .join(".local")
            .join("share"),
    };

    Ok(data_home
        .join(APPLICATION_DIRECTORY)
        .join(DATABASE_FILENAME))
}

fn absolute_environment_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

/// SQLite implementation of the application storage contract.
pub struct SqliteUsageStore {
    connection: Connection,
}

impl SqliteUsageStore {
    /// Opens (or creates) a database and applies all known schema migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    /// Creates the default data directory before opening its database.
    pub fn open_default() -> Result<Self, SqliteStoreError> {
        let path = default_database_path()?;
        let directory = path
            .parent()
            .ok_or_else(|| SqliteStoreError::InvalidDatabasePath(path.clone()))?;
        fs::create_dir_all(directory).map_err(|source| SqliteStoreError::CreateDataDirectory {
            path: directory.to_owned(),
            source,
        })?;
        Self::open(path)
    }

    pub fn open_in_memory() -> Result<Self, SqliteStoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, SqliteStoreError> {
        connection.pragma_update(None, "foreign_keys", true)?;
        migrate(&mut connection)?;
        Ok(Self { connection })
    }
}

impl UsageReadStore for SqliteUsageStore {
    type Error = SqliteStoreError;

    fn usage_snapshot(&self) -> Result<UsageSnapshot, Self::Error> {
        let transaction = self.connection.unchecked_transaction()?;
        let sessions = load_stored_sessions(&transaction)?;
        let observations = load_stored_observations(&transaction, &sessions)?;
        transaction.commit()?;
        let mut sessions = sessions.into_values().collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(UsageSnapshot {
            sessions,
            observations,
        })
    }
}

impl UsageStore for SqliteUsageStore {
    type Error = SqliteStoreError;

    fn source_states(&self, agent: &AgentId) -> Result<Vec<SourceState>, Self::Error> {
        let mut statement = self.connection.prepare(
            "SELECT path,
                    last_observed_size, last_observed_modified_seconds,
                    last_observed_modified_nanos,
                    last_imported_size, last_imported_modified_seconds,
                    last_imported_modified_nanos,
                    last_successful_scan_ms, last_parse_completion, present
               FROM sources
              WHERE agent = ?1
              ORDER BY path",
        )?;
        let rows = statement.query_map([agent.as_str()], source_state_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn record_discovery(
        &mut self,
        agent: &AgentId,
        report: &DiscoveryReport,
        observed_at: Timestamp,
    ) -> Result<(), Self::Error> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut discovered_paths = HashSet::with_capacity(report.files.len());

        for file in &report.files {
            let path = encode_path(&file.path);
            let (modified_seconds, modified_nanos) =
                system_time_to_parts(file.revision.modified_at)?;
            transaction.execute(
                "INSERT INTO sources (
                    path, last_observed_size, last_observed_modified_seconds,
                    last_observed_modified_nanos, last_discovery_scan_ms, present, agent
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
                 ON CONFLICT(agent, path) DO UPDATE SET
                    last_observed_size = excluded.last_observed_size,
                    last_observed_modified_seconds = excluded.last_observed_modified_seconds,
                    last_observed_modified_nanos = excluded.last_observed_modified_nanos,
                    last_discovery_scan_ms = excluded.last_discovery_scan_ms,
                    present = 1
                 WHERE excluded.last_discovery_scan_ms >= sources.last_discovery_scan_ms",
                params![
                    &path,
                    encode_u64(file.revision.size),
                    modified_seconds,
                    modified_nanos,
                    observed_at.as_unix_milliseconds(),
                    agent.as_str(),
                ],
            )?;
            discovered_paths.insert(path);
        }

        let stored_sources = {
            let mut statement =
                transaction.prepare("SELECT id, path FROM sources WHERE agent = ?1")?;
            let rows = statement.query_map([agent.as_str()], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        for (source_id, encoded_path) in stored_sources {
            if discovered_paths.contains(&encoded_path) {
                continue;
            }

            let path = decode_path(encoded_path);
            if discovery_covers(&path, report) {
                transaction.execute(
                    "UPDATE sources
                        SET present = 0, last_discovery_scan_ms = ?1
                      WHERE id = ?2 AND last_discovery_scan_ms <= ?1",
                    params![observed_at.as_unix_milliseconds(), source_id],
                )?;
            }
        }

        transaction.commit()?;
        Ok(())
    }

    fn commit_import(
        &mut self,
        import: &SessionImport,
    ) -> Result<CommitImportOutcome, Self::Error> {
        validate_import(import)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if import_is_stale(&transaction, import)? {
            return Ok(CommitImportOutcome::IgnoredStale);
        }

        let source_id = upsert_imported_source(&transaction, import)?;
        let source_session_id = upsert_source_session(&transaction, source_id, import)?;
        let mut stats = ImportStats::default();

        for event in &import.parsed.events {
            stats.event_identities_inserted += transaction.execute(
                "INSERT INTO usage_events (agent, adapter_key)
                 VALUES (?1, ?2)
                 ON CONFLICT(agent, adapter_key) DO NOTHING",
                params![event.identity.agent.as_str(), &event.identity.adapter_key],
            )? as u64;

            let event_id: i64 = transaction.query_row(
                "SELECT id FROM usage_events WHERE agent = ?1 AND adapter_key = ?2",
                params![event.identity.agent.as_str(), &event.identity.adapter_key],
                |row| row.get(0),
            )?;

            let inserted =
                insert_observation(&transaction, source_id, source_session_id, event_id, event)?;
            if inserted {
                stats.observations_inserted += 1;
            } else {
                stats.observations_updated += update_observation(
                    &transaction,
                    source_id,
                    source_session_id,
                    event_id,
                    event,
                )? as u64;
            }
        }

        transaction.commit()?;
        Ok(CommitImportOutcome::Applied(stats))
    }
}

fn source_state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceState> {
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
        present,
    })
}

fn revision_from_columns(
    row: &rusqlite::Row<'_>,
    size_column: usize,
    seconds_column: usize,
    nanos_column: usize,
) -> rusqlite::Result<Option<FileRevision>> {
    let size = row.get::<_, Option<Vec<u8>>>(size_column)?;
    let seconds = row.get::<_, Option<i64>>(seconds_column)?;
    let nanos = row.get::<_, Option<u32>>(nanos_column)?;
    match (size, seconds, nanos) {
        (None, None, None) => Ok(None),
        (Some(size), Some(seconds), Some(nanos)) => Ok(Some(FileRevision {
            size: decode_u64(&size).map_err(to_sql_conversion_error)?,
            modified_at: system_time_from_parts(seconds, nanos).map_err(to_sql_conversion_error)?,
        })),
        _ => Err(corrupt_sql_value("incomplete file revision")),
    }
}

fn discovery_covers(path: &Path, report: &DiscoveryReport) -> bool {
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

fn system_time_to_parts(value: SystemTime) -> Result<(i64, u32), SqliteStoreError> {
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

fn system_time_from_parts(seconds: i64, nanos: u32) -> Result<SystemTime, SqliteStoreError> {
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

fn encode_u64(value: u64) -> Vec<u8> {
    value.to_be_bytes().to_vec()
}

fn decode_u64(value: &[u8]) -> Result<u64, SqliteStoreError> {
    let bytes: [u8; 8] = value
        .try_into()
        .map_err(|_| SqliteStoreError::CorruptData("invalid stored unsigned integer"))?;
    Ok(u64::from_be_bytes(bytes))
}

fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

fn decode_path(value: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(OsString::from_vec(value))
}

fn encode_parent(parent: Option<&ParentSession>) -> (Option<&'static str>, Option<Vec<u8>>) {
    match parent {
        None => (None, None),
        Some(ParentSession::SessionId(id)) => (Some("session_id"), Some(id.as_bytes().to_vec())),
        Some(ParentSession::SourcePath(path)) => (Some("source_path"), Some(encode_path(path))),
    }
}

fn decode_parent(
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

fn attribution_parts(event: &UsageEvent) -> (Option<&str>, Option<&str>) {
    match &event.attribution {
        Some(attribution) => (Some(&attribution.provider), Some(&attribution.model)),
        None => (None, None),
    }
}

fn usage_kind_to_str(kind: UsageKind) -> &'static str {
    match kind {
        UsageKind::Assistant => "assistant",
        UsageKind::ToolResult => "tool_result",
        UsageKind::Compaction => "compaction",
        UsageKind::BranchSummary => "branch_summary",
        UsageKind::Other => "other",
    }
}

fn usage_kind_from_str(value: &str) -> Result<UsageKind, SqliteStoreError> {
    match value {
        "assistant" => Ok(UsageKind::Assistant),
        "tool_result" => Ok(UsageKind::ToolResult),
        "compaction" => Ok(UsageKind::Compaction),
        "branch_summary" => Ok(UsageKind::BranchSummary),
        "other" => Ok(UsageKind::Other),
        _ => Err(SqliteStoreError::CorruptData("an invalid usage kind")),
    }
}

fn completion_to_str(completion: ParseCompletion) -> &'static str {
    match completion {
        ParseCompletion::Complete => "complete",
        ParseCompletion::IncompleteFinalLine => "incomplete_final_line",
    }
}

fn completion_from_str(value: &str) -> Result<ParseCompletion, SqliteStoreError> {
    match value {
        "complete" => Ok(ParseCompletion::Complete),
        "incomplete_final_line" => Ok(ParseCompletion::IncompleteFinalLine),
        _ => Err(SqliteStoreError::CorruptData(
            "invalid stored parse completion",
        )),
    }
}

fn corrupt_sql_value(message: &'static str) -> rusqlite::Error {
    to_sql_conversion_error(SqliteStoreError::CorruptData(message))
}

fn to_sql_conversion_error(error: SqliteStoreError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(error))
}

#[derive(Debug)]
pub enum SqliteStoreError {
    Sqlite(rusqlite::Error),
    CreateDataDirectory { path: PathBuf, source: io::Error },
    HomeDirectoryUnavailable,
    InvalidDatabasePath(PathBuf),
    UnsupportedSchemaVersion(i64),
    ValueOutOfRange(&'static str),
    CorruptData(&'static str),
    InvalidImport(&'static str),
}

impl fmt::Display for SqliteStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(source) => write!(formatter, "SQLite storage error: {source}"),
            Self::CreateDataDirectory { path, source } => {
                write!(formatter, "could not create {}: {source}", path.display())
            }
            Self::HomeDirectoryUnavailable => formatter.write_str(
                "HOME is unavailable or invalid and XDG_DATA_HOME is not an absolute path",
            ),
            Self::InvalidDatabasePath(path) => {
                write!(formatter, "database path has no parent: {}", path.display())
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(formatter, "unsupported database schema version {version}")
            }
            Self::ValueOutOfRange(value) => write!(formatter, "{value} is out of range"),
            Self::CorruptData(message) => write!(formatter, "database contains {message}"),
            Self::InvalidImport(message) => write!(formatter, "invalid session import: {message}"),
        }
    }
}

impl Error for SqliteStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(source) => Some(source),
            Self::CreateDataDirectory { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for SqliteStoreError {
    fn from(source: rusqlite::Error) -> Self {
        Self::Sqlite(source)
    }
}
