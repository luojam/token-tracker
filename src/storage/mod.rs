mod billing;
mod codec;
mod error;
mod paths;

use codec::*;
pub use error::SqliteStoreError;
pub use paths::default_database_path;
mod importing;
mod parse_notices;
mod reading;
mod schema;

use importing::{
    import_is_stale, insert_observation, normalization_changed, update_observation,
    upsert_imported_source, upsert_source_session,
};
use reading::{load_stored_observations, load_stored_sessions};
use schema::migrate;

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use rusqlite::{Connection, TransactionBehavior, params};

use crate::application::{
    CommitImportOutcome, DiscoveryReport, ImportStats, ParseCompletion, SourceState,
    UsageReadStore, UsageSnapshot, UsageStore, ValidatedSessionImport,
};
use crate::domain::{AgentId, Timestamp};

pub struct SqliteUsageStore {
    connection: Connection,
}

impl SqliteUsageStore {
    /// Opens (or creates) a database and applies schema migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

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
                    last_successful_scan_ms, last_parse_completion, present, parse_notices, normalization_version
               FROM import_sources
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
                "INSERT INTO import_sources (
                    path, last_observed_size, last_observed_modified_seconds,
                    last_observed_modified_nanos, last_discovery_scan_ms, present, agent
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
                 ON CONFLICT(agent, path) DO UPDATE SET
                    last_observed_size = excluded.last_observed_size,
                    last_observed_modified_seconds = excluded.last_observed_modified_seconds,
                    last_observed_modified_nanos = excluded.last_observed_modified_nanos,
                    last_discovery_scan_ms = excluded.last_discovery_scan_ms,
                    present = 1
                 WHERE excluded.last_discovery_scan_ms >= import_sources.last_discovery_scan_ms",
                params![
                    &path,
                    encode_u64(file.revision.size)?,
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
                transaction.prepare("SELECT id, path FROM import_sources WHERE agent = ?1")?;
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
                    "UPDATE import_sources
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
        import: &ValidatedSessionImport,
    ) -> Result<CommitImportOutcome, Self::Error> {
        let import = import.as_import();

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if import_is_stale(&transaction, import)? {
            return Ok(CommitImportOutcome::IgnoredStale);
        }

        if import.parsed.completion != ParseCompletion::Complete
            && normalization_changed(&transaction, import)?
        {
            return Ok(CommitImportOutcome::DeferredIncomplete);
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
