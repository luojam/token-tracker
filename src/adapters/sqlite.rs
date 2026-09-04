//! SQLite persistence for imported session metadata and usage observations.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::application::{
    CommitImportOutcome, DiscoveryReport, FileRevision, ImportStats, ParseCompletion,
    SessionImport, SourceState, UsageStore, UsageSummaryStore,
};
use crate::core::{
    ModelAttribution, RecordedCost, SummaryBreakdown, SummaryGroup, SummaryTotals, Timestamp,
    TokenCounts, UsageEvent, UsageKind, UsageSummary,
};

const SCHEMA_VERSION: i64 = 2;
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

impl UsageSummaryStore for SqliteUsageStore {
    type Error = SqliteStoreError;

    fn all_time_summary(&self) -> Result<UsageSummary, Self::Error> {
        let sessions = load_stored_sessions(&self.connection)?;
        let session_count = sessions
            .values()
            .map(|session| (&session.agent, &session.session_id))
            .collect::<HashSet<_>>()
            .len();
        let parents = resolve_session_parents(&sessions);
        let observations = load_stored_observations(&self.connection)?;
        let mut observations_by_event = BTreeMap::<EventKey, Vec<StoredObservation>>::new();

        for observation in observations {
            let session = sessions.get(&observation.source_session_id).ok_or(
                SqliteStoreError::CorruptData("an observation without session provenance"),
            )?;
            if session.agent != observation.event.agent {
                return Err(SqliteStoreError::CorruptData(
                    "an observation whose event and session agents differ",
                ));
            }
            observations_by_event
                .entry(observation.event.clone())
                .or_default()
                .push(observation);
        }

        let mut totals = SummaryTotals {
            session_count: u64::try_from(session_count)
                .map_err(|_| SqliteStoreError::ValueOutOfRange("session count"))?,
            unique_usage_event_count: u64::try_from(observations_by_event.len())
                .map_err(|_| SqliteStoreError::ValueOutOfRange("usage event count"))?,
            ..SummaryTotals::default()
        };
        let mut breakdown = BTreeMap::<SummaryGroup, SummaryAccumulator>::new();

        // Event keys provide an aggregation order that does not depend on row or
        // import order. This also makes floating-point cost output deterministic.
        for event_observations in observations_by_event.values() {
            let canonical = select_canonical_observation(event_observations, &sessions, &parents)?;
            totals.tokens = checked_add_tokens(totals.tokens, canonical.tokens)?;
            checked_add_cost(&mut totals.recorded_cost, canonical.recorded_cost)?;

            let group = match &canonical.attribution {
                Some(attribution) => SummaryGroup::ProviderModel(attribution.clone()),
                None => SummaryGroup::Unattributed(canonical.kind),
            };
            let group_totals = breakdown.entry(group).or_default();
            group_totals.tokens = checked_add_tokens(group_totals.tokens, canonical.tokens)?;
            checked_add_cost(&mut group_totals.recorded_cost, canonical.recorded_cost)?;
            group_totals.unique_usage_event_count = group_totals
                .unique_usage_event_count
                .checked_add(1)
                .ok_or(SqliteStoreError::ValueOutOfRange("usage event count"))?;
        }

        Ok(UsageSummary {
            totals,
            breakdown: breakdown
                .into_iter()
                .map(|(group, totals)| SummaryBreakdown {
                    group,
                    tokens: totals.tokens,
                    recorded_cost: totals.recorded_cost,
                    unique_usage_event_count: totals.unique_usage_event_count,
                })
                .collect(),
        })
    }
}

impl UsageStore for SqliteUsageStore {
    type Error = SqliteStoreError;

    fn source_states(&self) -> Result<Vec<SourceState>, Self::Error> {
        let mut statement = self.connection.prepare(
            "SELECT path,
                    last_observed_size, last_observed_modified_seconds,
                    last_observed_modified_nanos,
                    last_imported_size, last_imported_modified_seconds,
                    last_imported_modified_nanos,
                    last_successful_scan_ms, last_parse_completion, present,
                    reimport_required
               FROM sources
              ORDER BY path",
        )?;
        let rows = statement.query_map([], source_state_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn record_discovery(
        &mut self,
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
                    last_observed_modified_nanos, last_discovery_scan_ms, present
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1)
                 ON CONFLICT(path) DO UPDATE SET
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
                ],
            )?;
            discovered_paths.insert(path);
        }

        let stored_sources = {
            let mut statement = transaction.prepare("SELECT id, path FROM sources")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        for (source_id, encoded_path) in stored_sources {
            if discovered_paths.contains(&encoded_path) {
                continue;
            }

            let path = decode_path(encoded_path)?;
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EventKey {
    agent: String,
    adapter_key: String,
}

#[derive(Clone, Debug)]
struct StoredSession {
    id: i64,
    agent: String,
    session_id: String,
    started_at_ms: i64,
    parent_session: Option<String>,
    source_path: PathBuf,
}

#[derive(Clone, Debug)]
struct StoredObservation {
    event: EventKey,
    source_session_id: i64,
    session_provenance_known: bool,
    kind: UsageKind,
    attribution: Option<ModelAttribution>,
    tokens: TokenCounts,
    recorded_cost: Option<RecordedCost>,
}

#[derive(Clone, Copy, Debug, Default)]
struct SummaryAccumulator {
    tokens: TokenCounts,
    recorded_cost: Option<RecordedCost>,
    unique_usage_event_count: u64,
}

fn load_stored_sessions(
    connection: &Connection,
) -> Result<HashMap<i64, StoredSession>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT session.id, session.agent, session.session_id,
                session.started_at_ms, session.parent_session, source.path
           FROM source_sessions session
           JOIN sources source ON source.id = session.source_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Vec<u8>>(5)?,
        ))
    })?;

    let mut sessions = HashMap::new();
    for row in rows {
        let (id, agent, session_id, started_at_ms, parent_session, source_path) = row?;
        sessions.insert(
            id,
            StoredSession {
                id,
                agent,
                session_id,
                started_at_ms,
                parent_session,
                source_path: decode_path(source_path)?,
            },
        );
    }
    Ok(sessions)
}

fn resolve_session_parents(sessions: &HashMap<i64, StoredSession>) -> HashMap<i64, i64> {
    let mut sessions_by_source = HashMap::<(String, PathBuf), Vec<i64>>::new();
    for session in sessions.values() {
        sessions_by_source
            .entry((session.agent.clone(), session.source_path.clone()))
            .or_default()
            .push(session.id);
    }

    sessions
        .values()
        .filter_map(|session| {
            let parent_path = PathBuf::from(session.parent_session.as_deref()?);
            let candidates = sessions_by_source.get(&(session.agent.clone(), parent_path))?;
            match candidates.as_slice() {
                [parent_id] if *parent_id != session.id => Some((session.id, *parent_id)),
                _ => None,
            }
        })
        .collect()
}

fn load_stored_observations(
    connection: &Connection,
) -> Result<Vec<StoredObservation>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT event.agent, event.adapter_key, observation.source_session_id,
                observation.session_provenance_known, observation.usage_kind,
                observation.provider, observation.model,
                observation.input_tokens, observation.output_tokens,
                observation.cache_read_tokens, observation.cache_write_tokens,
                observation.recorded_cost_usd
           FROM source_observations observation
           JOIN usage_events event ON event.id = observation.event_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, Vec<u8>>(7)?,
            row.get::<_, Vec<u8>>(8)?,
            row.get::<_, Vec<u8>>(9)?,
            row.get::<_, Vec<u8>>(10)?,
            row.get::<_, Option<f64>>(11)?,
        ))
    })?;

    let mut observations = Vec::new();
    for row in rows {
        let (
            agent,
            adapter_key,
            source_session_id,
            session_provenance_known,
            kind,
            provider,
            model,
            input,
            output,
            cache_read,
            cache_write,
            recorded_cost,
        ) = row?;
        let session_provenance_known = match session_provenance_known {
            0 => false,
            1 => true,
            _ => {
                return Err(SqliteStoreError::CorruptData(
                    "an invalid observation provenance state",
                ));
            }
        };
        let attribution = match (provider, model) {
            (Some(provider), Some(model)) => Some(ModelAttribution { provider, model }),
            (None, None) => None,
            _ => {
                return Err(SqliteStoreError::CorruptData(
                    "an incomplete model attribution",
                ));
            }
        };
        let recorded_cost = recorded_cost
            .map(RecordedCost::from_usd)
            .transpose()
            .map_err(|_| SqliteStoreError::CorruptData("an invalid recorded cost"))?;

        observations.push(StoredObservation {
            event: EventKey { agent, adapter_key },
            source_session_id,
            session_provenance_known,
            kind: usage_kind_from_str(&kind)?,
            attribution,
            tokens: TokenCounts {
                input: decode_u64(&input)?,
                output: decode_u64(&output)?,
                cache_read: decode_u64(&cache_read)?,
                cache_write: decode_u64(&cache_write)?,
            },
            recorded_cost,
        });
    }
    Ok(observations)
}

fn select_canonical_observation<'a>(
    observations: &'a [StoredObservation],
    sessions: &HashMap<i64, StoredSession>,
    parents: &HashMap<i64, i64>,
) -> Result<&'a StoredObservation, SqliteStoreError> {
    let mut candidates = observations
        .iter()
        .filter(|candidate| {
            !observations.iter().any(|other| {
                other.source_session_id != candidate.source_session_id
                    && observation_is_ancestor(other, candidate, parents)
            })
        })
        .collect::<Vec<_>>();

    // Invalid cyclic lineage can make every observation appear dominated. Treat
    // it as ambiguous and use the documented stable fallback instead.
    if candidates.is_empty() {
        candidates.extend(observations);
    }

    candidates
        .into_iter()
        .min_by(|left, right| {
            let left_session = sessions.get(&left.source_session_id);
            let right_session = sessions.get(&right.source_session_id);
            match (left_session, right_session) {
                (Some(left), Some(right)) => {
                    observation_fallback_key(left).cmp(&observation_fallback_key(right))
                }
                _ => std::cmp::Ordering::Equal,
            }
        })
        .ok_or(SqliteStoreError::CorruptData(
            "a usage event without observations",
        ))
}

fn observation_is_ancestor(
    possible_ancestor: &StoredObservation,
    possible_descendant: &StoredObservation,
    parents: &HashMap<i64, i64>,
) -> bool {
    if !possible_ancestor.session_provenance_known || !possible_descendant.session_provenance_known
    {
        return false;
    }

    let mut current = possible_descendant.source_session_id;
    let mut visited = HashSet::new();
    let mut ancestors = Vec::new();
    while let Some(parent) = parents.get(&current).copied() {
        if !visited.insert(current) {
            return false;
        }
        ancestors.push(parent);
        current = parent;
    }
    ancestors.contains(&possible_ancestor.source_session_id)
}

fn observation_fallback_key(session: &StoredSession) -> (i64, &str, &Path) {
    (
        session.started_at_ms,
        &session.session_id,
        &session.source_path,
    )
}

fn checked_add_tokens(
    current: TokenCounts,
    value: TokenCounts,
) -> Result<TokenCounts, SqliteStoreError> {
    current
        .checked_add(value)
        .ok_or(SqliteStoreError::ValueOutOfRange("summary token total"))
}

fn checked_add_cost(
    current: &mut Option<RecordedCost>,
    value: Option<RecordedCost>,
) -> Result<(), SqliteStoreError> {
    let Some(value) = value else {
        return Ok(());
    };
    *current = Some(match *current {
        Some(current) => current
            .checked_add(value)
            .map_err(|_| SqliteStoreError::ValueOutOfRange("summary recorded cost"))?,
        None => value,
    });
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), SqliteStoreError> {
    let mut version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(SqliteStoreError::UnsupportedSchemaVersion(version));
    }

    if version == 0 {
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE sources (
                id INTEGER PRIMARY KEY,
                path BLOB NOT NULL UNIQUE,
                agent TEXT,
                session_id TEXT,
                format_version INTEGER,
                working_directory BLOB,
                started_at_ms INTEGER,
                name TEXT,
                parent_session TEXT,
                last_observed_size BLOB NOT NULL,
                last_observed_modified_seconds INTEGER NOT NULL,
                last_observed_modified_nanos INTEGER NOT NULL,
                last_imported_size BLOB,
                last_imported_modified_seconds INTEGER,
                last_imported_modified_nanos INTEGER,
                last_discovery_scan_ms INTEGER NOT NULL,
                last_successful_scan_ms INTEGER,
                last_parse_completion TEXT,
                present INTEGER NOT NULL CHECK (present IN (0, 1)),
                CHECK (last_observed_modified_nanos BETWEEN 0 AND 999999999),
                CHECK (
                    (last_imported_size IS NULL
                     AND last_imported_modified_seconds IS NULL
                     AND last_imported_modified_nanos IS NULL)
                    OR
                    (last_imported_size IS NOT NULL
                     AND last_imported_modified_seconds IS NOT NULL
                     AND last_imported_modified_nanos BETWEEN 0 AND 999999999)
                )
            );

            CREATE TABLE usage_events (
                id INTEGER PRIMARY KEY,
                agent TEXT NOT NULL,
                adapter_key TEXT NOT NULL,
                UNIQUE (agent, adapter_key)
            );

            CREATE TABLE source_observations (
                source_id INTEGER NOT NULL REFERENCES sources(id),
                event_id INTEGER NOT NULL REFERENCES usage_events(id),
                timestamp_ms INTEGER NOT NULL,
                usage_kind TEXT NOT NULL,
                provider TEXT,
                model TEXT,
                input_tokens BLOB NOT NULL,
                output_tokens BLOB NOT NULL,
                cache_read_tokens BLOB NOT NULL,
                cache_write_tokens BLOB NOT NULL,
                recorded_cost_usd REAL,
                PRIMARY KEY (source_id, event_id),
                CHECK ((provider IS NULL) = (model IS NULL)),
                CHECK (recorded_cost_usd IS NULL OR recorded_cost_usd >= 0.0)
            );

            CREATE INDEX source_observations_event
                ON source_observations(event_id);

            PRAGMA user_version = 1;",
        )?;
        transaction.commit()?;
        version = 1;
    }

    if version == 1 {
        // Version 1 retained observations across source rewrites but only kept the
        // latest session metadata. Mark those associations as unknown and force
        // a reimport so events still present in available files regain provenance.
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "CREATE TABLE source_sessions (
                id INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL REFERENCES sources(id),
                agent TEXT NOT NULL,
                session_id TEXT NOT NULL,
                format_version INTEGER NOT NULL,
                working_directory BLOB NOT NULL,
                started_at_ms INTEGER NOT NULL,
                name TEXT,
                parent_session TEXT,
                UNIQUE (source_id, agent, session_id),
                UNIQUE (id, source_id)
            );

            CREATE INDEX source_sessions_identity
                ON source_sessions(agent, session_id);

            INSERT INTO source_sessions (
                source_id, agent, session_id, format_version, working_directory,
                started_at_ms, name, parent_session
            )
            SELECT id, agent, session_id, format_version, working_directory,
                   started_at_ms, name, parent_session
              FROM sources
             WHERE agent IS NOT NULL;

            DROP INDEX source_observations_event;
            ALTER TABLE source_observations RENAME TO source_observations_v1;

            CREATE TABLE source_observations (
                source_id INTEGER NOT NULL REFERENCES sources(id),
                source_session_id INTEGER NOT NULL,
                event_id INTEGER NOT NULL REFERENCES usage_events(id),
                timestamp_ms INTEGER NOT NULL,
                usage_kind TEXT NOT NULL,
                provider TEXT,
                model TEXT,
                input_tokens BLOB NOT NULL,
                output_tokens BLOB NOT NULL,
                cache_read_tokens BLOB NOT NULL,
                cache_write_tokens BLOB NOT NULL,
                recorded_cost_usd REAL,
                session_provenance_known INTEGER NOT NULL
                    CHECK (session_provenance_known IN (0, 1)),
                PRIMARY KEY (source_session_id, event_id),
                FOREIGN KEY (source_session_id, source_id)
                    REFERENCES source_sessions(id, source_id),
                CHECK ((provider IS NULL) = (model IS NULL)),
                CHECK (recorded_cost_usd IS NULL OR recorded_cost_usd >= 0.0)
            );

            INSERT INTO source_observations (
                source_id, source_session_id, event_id, timestamp_ms, usage_kind,
                provider, model, input_tokens, output_tokens, cache_read_tokens,
                cache_write_tokens, recorded_cost_usd, session_provenance_known
            )
            SELECT observation.source_id, session.id, observation.event_id,
                   observation.timestamp_ms, observation.usage_kind,
                   observation.provider, observation.model,
                   observation.input_tokens, observation.output_tokens,
                   observation.cache_read_tokens, observation.cache_write_tokens,
                   observation.recorded_cost_usd, 0
              FROM source_observations_v1 observation
              JOIN source_sessions session
                ON session.source_id = observation.source_id;

            DROP TABLE source_observations_v1;

            CREATE INDEX source_observations_event
                ON source_observations(event_id);

            ALTER TABLE sources ADD COLUMN reimport_required INTEGER NOT NULL DEFAULT 0
                CHECK (reimport_required IN (0, 1));

            UPDATE sources
               SET reimport_required = 1
             WHERE agent IS NOT NULL;

            PRAGMA user_version = 2;",
        )?;
        transaction.commit()?;
    }

    Ok(())
}

fn upsert_imported_source(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<i64, SqliteStoreError> {
    let path = encode_path(&import.source.path);
    let working_directory = encode_path(&import.parsed.metadata.working_directory);
    let (modified_seconds, modified_nanos) =
        system_time_to_parts(import.source.revision.modified_at)?;
    let size = encode_u64(import.source.revision.size);

    transaction.execute(
        "INSERT INTO sources (
            path, agent, session_id, format_version, working_directory,
            started_at_ms, name, parent_session,
            last_observed_size, last_observed_modified_seconds,
            last_observed_modified_nanos,
            last_imported_size, last_imported_modified_seconds,
            last_imported_modified_nanos,
            last_discovery_scan_ms, last_successful_scan_ms,
            last_parse_completion, present
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?9, ?10, ?11, ?12, ?12, ?13, 1
         )
         ON CONFLICT(path) DO UPDATE SET
            agent = excluded.agent,
            session_id = excluded.session_id,
            format_version = excluded.format_version,
            working_directory = excluded.working_directory,
            started_at_ms = excluded.started_at_ms,
            name = excluded.name,
            parent_session = excluded.parent_session,
            last_observed_size = CASE
                WHEN excluded.last_discovery_scan_ms >= sources.last_discovery_scan_ms
                THEN excluded.last_observed_size ELSE sources.last_observed_size END,
            last_observed_modified_seconds = CASE
                WHEN excluded.last_discovery_scan_ms >= sources.last_discovery_scan_ms
                THEN excluded.last_observed_modified_seconds
                ELSE sources.last_observed_modified_seconds END,
            last_observed_modified_nanos = CASE
                WHEN excluded.last_discovery_scan_ms >= sources.last_discovery_scan_ms
                THEN excluded.last_observed_modified_nanos
                ELSE sources.last_observed_modified_nanos END,
            last_imported_size = excluded.last_imported_size,
            last_imported_modified_seconds = excluded.last_imported_modified_seconds,
            last_imported_modified_nanos = excluded.last_imported_modified_nanos,
            last_discovery_scan_ms = MAX(
                sources.last_discovery_scan_ms,
                excluded.last_discovery_scan_ms
            ),
            last_successful_scan_ms = excluded.last_successful_scan_ms,
            last_parse_completion = excluded.last_parse_completion,
            reimport_required = 0,
            present = CASE
                WHEN excluded.last_discovery_scan_ms >= sources.last_discovery_scan_ms
                THEN 1 ELSE sources.present END",
        params![
            &path,
            import.parsed.metadata.agent.as_str(),
            &import.parsed.metadata.session_id,
            i64::from(import.parsed.metadata.format_version),
            working_directory,
            import.parsed.metadata.started_at.as_unix_milliseconds(),
            &import.parsed.metadata.name,
            &import.parsed.metadata.parent_session,
            size,
            modified_seconds,
            modified_nanos,
            import.scanned_at.as_unix_milliseconds(),
            completion_to_str(import.parsed.completion),
        ],
    )?;

    transaction
        .query_row(
            "SELECT id FROM sources WHERE path = ?1",
            params![path],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn upsert_source_session(
    transaction: &Transaction<'_>,
    source_id: i64,
    import: &SessionImport,
) -> Result<i64, SqliteStoreError> {
    let metadata = &import.parsed.metadata;
    transaction.execute(
        "INSERT INTO source_sessions (
            source_id, agent, session_id, format_version, working_directory,
            started_at_ms, name, parent_session
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(source_id, agent, session_id) DO UPDATE SET
            format_version = excluded.format_version,
            working_directory = excluded.working_directory,
            started_at_ms = excluded.started_at_ms,
            name = excluded.name,
            parent_session = excluded.parent_session",
        params![
            source_id,
            metadata.agent.as_str(),
            &metadata.session_id,
            i64::from(metadata.format_version),
            encode_path(&metadata.working_directory),
            metadata.started_at.as_unix_milliseconds(),
            &metadata.name,
            &metadata.parent_session,
        ],
    )?;

    transaction
        .query_row(
            "SELECT id
               FROM source_sessions
              WHERE source_id = ?1 AND agent = ?2 AND session_id = ?3",
            params![source_id, metadata.agent.as_str(), &metadata.session_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn import_is_stale(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<bool, SqliteStoreError> {
    let path = encode_path(&import.source.path);
    let stored = transaction
        .query_row(
            "SELECT last_successful_scan_ms, last_discovery_scan_ms
               FROM sources
              WHERE path = ?1",
            params![path],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;

    let Some((last_successful_scan, last_discovery_scan)) = stored else {
        return Ok(false);
    };

    let scanned_at = import.scanned_at.as_unix_milliseconds();
    Ok(last_discovery_scan > scanned_at
        || last_successful_scan.is_some_and(|last_scan| last_scan > scanned_at))
}

fn insert_observation(
    transaction: &Transaction<'_>,
    source_id: i64,
    source_session_id: i64,
    event_id: i64,
    event: &UsageEvent,
) -> Result<bool, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    let changed = transaction.execute(
        "INSERT INTO source_observations (
            source_id, source_session_id, event_id, timestamp_ms, usage_kind,
            provider, model, input_tokens, output_tokens, cache_read_tokens,
            cache_write_tokens, recorded_cost_usd, session_provenance_known
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1)
         ON CONFLICT(source_session_id, event_id) DO NOTHING",
        params![
            source_id,
            source_session_id,
            event_id,
            event.timestamp.as_unix_milliseconds(),
            usage_kind_to_str(event.kind),
            provider,
            model,
            encode_u64(event.tokens.input),
            encode_u64(event.tokens.output),
            encode_u64(event.tokens.cache_read),
            encode_u64(event.tokens.cache_write),
            event.recorded_cost.map(RecordedCost::as_usd),
        ],
    )?;
    Ok(changed == 1)
}

fn update_observation(
    transaction: &Transaction<'_>,
    source_id: i64,
    source_session_id: i64,
    event_id: i64,
    event: &UsageEvent,
) -> Result<usize, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    transaction
        .execute(
            "UPDATE source_observations
                SET timestamp_ms = ?1,
                    usage_kind = ?2,
                    provider = ?3,
                    model = ?4,
                    input_tokens = ?5,
                    output_tokens = ?6,
                    cache_read_tokens = ?7,
                    cache_write_tokens = ?8,
                    recorded_cost_usd = ?9,
                    session_provenance_known = 1
              WHERE source_id = ?10 AND source_session_id = ?11 AND event_id = ?12
                AND (session_provenance_known != 1
                     OR timestamp_ms IS NOT ?1
                     OR usage_kind IS NOT ?2
                     OR provider IS NOT ?3
                     OR model IS NOT ?4
                     OR input_tokens IS NOT ?5
                     OR output_tokens IS NOT ?6
                     OR cache_read_tokens IS NOT ?7
                     OR cache_write_tokens IS NOT ?8
                     OR recorded_cost_usd IS NOT ?9)",
            params![
                event.timestamp.as_unix_milliseconds(),
                usage_kind_to_str(event.kind),
                provider,
                model,
                encode_u64(event.tokens.input),
                encode_u64(event.tokens.output),
                encode_u64(event.tokens.cache_read),
                encode_u64(event.tokens.cache_write),
                event.recorded_cost.map(RecordedCost::as_usd),
                source_id,
                source_session_id,
                event_id,
            ],
        )
        .map_err(Into::into)
}

fn validate_import(import: &SessionImport) -> Result<(), SqliteStoreError> {
    if import
        .parsed
        .events
        .iter()
        .any(|event| event.identity.agent.as_str() != import.parsed.metadata.agent.as_str())
    {
        return Err(SqliteStoreError::InvalidImport(
            "an event agent does not match its session agent",
        ));
    }
    Ok(())
}

fn source_state_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceState> {
    let path = decode_path(row.get(0)?).map_err(to_sql_conversion_error)?;
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
    let reimport_required = match row.get::<_, i64>(10)? {
        0 => false,
        1 => true,
        _ => return Err(corrupt_sql_value("invalid source reimport state")),
    };

    Ok(SourceState {
        path,
        last_observed_revision,
        last_imported_revision,
        last_successful_scan,
        last_parse_completion,
        present,
        reimport_required,
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

#[cfg(unix)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
fn decode_path(value: Vec<u8>) -> Result<PathBuf, SqliteStoreError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_vec(value)))
}

#[cfg(windows)]
fn encode_path(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
fn decode_path(value: Vec<u8>) -> Result<PathBuf, SqliteStoreError> {
    use std::os::windows::ffi::OsStringExt;
    if value.len() % 2 != 0 {
        return Err(SqliteStoreError::CorruptData("invalid stored path"));
    }
    let wide = value
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    Ok(PathBuf::from(OsString::from_wide(&wide)))
}

#[cfg(not(any(unix, windows)))]
fn encode_path(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

#[cfg(not(any(unix, windows)))]
fn decode_path(value: Vec<u8>) -> Result<PathBuf, SqliteStoreError> {
    String::from_utf8(value)
        .map(PathBuf::from)
        .map_err(|_| SqliteStoreError::CorruptData("invalid stored path"))
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
    }
}

fn usage_kind_from_str(value: &str) -> Result<UsageKind, SqliteStoreError> {
    match value {
        "assistant" => Ok(UsageKind::Assistant),
        "tool_result" => Ok(UsageKind::ToolResult),
        "compaction" => Ok(UsageKind::Compaction),
        "branch_summary" => Ok(UsageKind::BranchSummary),
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
                write!(
                    formatter,
                    "database schema version {version} is newer than supported"
                )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{
        DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, ParsedSession,
    };
    use crate::core::{
        AgentId, ModelAttribution, SessionMetadata, TokenCounts, UsageEventIdentity,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_DATABASE: AtomicU64 = AtomicU64::new(0);

    struct TempDatabase {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempDatabase {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DATABASE.fetch_add(1, Ordering::Relaxed);
            let directory = env::temp_dir().join(format!(
                "token-tracker-sqlite-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&directory).unwrap();
            let path = directory.join("usage.db");
            Self { directory, path }
        }
    }

    impl Drop for TempDatabase {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn session_import(path: &str, input_tokens: u64) -> SessionImport {
        SessionImport {
            source: DiscoveredSessionFile {
                path: PathBuf::from(path),
                revision: FileRevision {
                    size: 123,
                    modified_at: UNIX_EPOCH + Duration::new(1_700_000_000, 123),
                },
            },
            scanned_at: Timestamp::from_unix_milliseconds(1_700_000_001_000),
            parsed: ParsedSession {
                metadata: SessionMetadata {
                    agent: AgentId::from("pi"),
                    session_id: format!("session-{path}"),
                    format_version: 3,
                    working_directory: PathBuf::from("/work/project"),
                    started_at: Timestamp::from_unix_milliseconds(1_700_000_000_000),
                    name: Some("Stored session".into()),
                    parent_session: None,
                },
                events: vec![UsageEvent {
                    identity: UsageEventIdentity {
                        agent: AgentId::from("pi"),
                        adapter_key: "shared-event".into(),
                    },
                    timestamp: Timestamp::from_unix_milliseconds(1_700_000_000_500),
                    kind: UsageKind::Assistant,
                    attribution: Some(ModelAttribution {
                        provider: "provider".into(),
                        model: "model".into(),
                    }),
                    tokens: TokenCounts {
                        input: input_tokens,
                        output: 2,
                        cache_read: 3,
                        cache_write: 4,
                    },
                    recorded_cost: Some(RecordedCost::from_usd(0.25).unwrap()),
                }],
                completion: ParseCompletion::Complete,
            },
        }
    }

    #[test]
    fn default_path_prefers_xdg_and_falls_back_to_home() {
        assert_eq!(
            default_database_path_from(Some(OsStr::new("/data")), Some(OsStr::new("/home/me")))
                .unwrap(),
            PathBuf::from("/data/token-tracker/usage.db")
        );
        assert_eq!(
            default_database_path_from(None, Some(OsStr::new("/home/me"))).unwrap(),
            PathBuf::from("/home/me/.local/share/token-tracker/usage.db")
        );
        assert_eq!(
            default_database_path_from(
                Some(OsStr::new("relative-data")),
                Some(OsStr::new("/home/me")),
            )
            .unwrap(),
            PathBuf::from("/home/me/.local/share/token-tracker/usage.db")
        );
    }

    #[test]
    fn default_path_rejects_an_invalid_home() {
        for home in [OsStr::new(""), OsStr::new("relative-home")] {
            assert!(matches!(
                default_database_path_from(None, Some(home)),
                Err(SqliteStoreError::HomeDirectoryUnavailable)
            ));
        }
    }

    #[test]
    fn repeated_import_is_idempotent_and_source_values_can_change() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let original = session_import("/sessions/a.jsonl", 10);

        assert_eq!(
            store.commit_import(&original).unwrap(),
            CommitImportOutcome::Applied(ImportStats {
                event_identities_inserted: 1,
                observations_inserted: 1,
                observations_updated: 0,
            })
        );
        assert_eq!(
            store.commit_import(&original).unwrap(),
            CommitImportOutcome::Applied(ImportStats::default())
        );

        let mut repeated = original.clone();
        repeated.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
        assert_eq!(
            store.commit_import(&repeated).unwrap(),
            CommitImportOutcome::Applied(ImportStats::default())
        );

        let mut changed = session_import("/sessions/a.jsonl", 99);
        changed.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
        assert_eq!(
            store.commit_import(&changed).unwrap(),
            CommitImportOutcome::Applied(ImportStats {
                event_identities_inserted: 0,
                observations_inserted: 0,
                observations_updated: 1,
            })
        );
    }

    #[test]
    fn a_newer_session_can_replace_metadata_at_the_same_source_path() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let original = session_import("/sessions/a.jsonl", 10);
        store.commit_import(&original).unwrap();

        let mut replacement = session_import("/sessions/a.jsonl", 99);
        replacement.parsed.metadata.session_id = "replacement-session".into();
        replacement.parsed.metadata.working_directory = PathBuf::from("/work/replacement");
        replacement.parsed.metadata.started_at =
            Timestamp::from_unix_milliseconds(1_700_000_001_000);
        replacement.parsed.metadata.name = Some("Replacement session".into());
        replacement.parsed.metadata.parent_session = Some("parent-session".into());
        replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);

        assert_eq!(
            store.commit_import(&replacement).unwrap(),
            CommitImportOutcome::Applied(ImportStats {
                event_identities_inserted: 0,
                observations_inserted: 1,
                observations_updated: 0,
            })
        );
        let stored_metadata = store
            .connection
            .query_row(
                "SELECT session_id, working_directory, started_at_ms, name, parent_session
                   FROM sources",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored_metadata.0, "replacement-session");
        assert_eq!(
            decode_path(stored_metadata.1).unwrap(),
            PathBuf::from("/work/replacement")
        );
        assert_eq!(stored_metadata.2, 1_700_000_001_000);
        assert_eq!(stored_metadata.3.as_deref(), Some("Replacement session"));
        assert_eq!(stored_metadata.4.as_deref(), Some("parent-session"));
        let mut statement = store
            .connection
            .prepare("SELECT input_tokens FROM source_observations ORDER BY input_tokens")
            .unwrap();
        let stored_tokens = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|value| decode_u64(&value.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(stored_tokens, vec![10, 99]);
    }

    #[test]
    fn a_late_import_from_the_replaced_session_is_ignored() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let original = session_import("/sessions/a.jsonl", 10);
        store.commit_import(&original).unwrap();

        let mut replacement = session_import("/sessions/a.jsonl", 99);
        replacement.parsed.metadata.session_id = "replacement-session".into();
        replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
        store.commit_import(&replacement).unwrap();

        let mut late_original = original;
        late_original.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
        assert_eq!(
            store.commit_import(&late_original).unwrap(),
            CommitImportOutcome::IgnoredStale
        );

        let stored_session: String = store
            .connection
            .query_row("SELECT session_id FROM sources", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_session, "replacement-session");
        let mut statement = store
            .connection
            .prepare("SELECT input_tokens FROM source_observations ORDER BY input_tokens")
            .unwrap();
        let stored_tokens = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|value| decode_u64(&value.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(stored_tokens, vec![10, 99]);
    }

    #[test]
    fn a_stale_first_import_cannot_claim_a_newer_discovered_source() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let stale = session_import("/sessions/a.jsonl", 10);
        store
            .record_discovery(
                &DiscoveryReport {
                    files: vec![stale.source.clone()],
                    warnings: Vec::new(),
                    coverage: DiscoveryCoverage {
                        inspected_roots: vec![PathBuf::from("/sessions")],
                        inaccessible_paths: Vec::new(),
                    },
                },
                Timestamp::from_unix_milliseconds(1_700_000_003_000),
            )
            .unwrap();
        assert_eq!(
            store.commit_import(&stale).unwrap(),
            CommitImportOutcome::IgnoredStale
        );

        let mut current = session_import("/sessions/a.jsonl", 99);
        current.parsed.metadata.session_id = "current-session".into();
        current.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_004_000);
        assert_eq!(
            store.commit_import(&current).unwrap(),
            CommitImportOutcome::Applied(ImportStats {
                event_identities_inserted: 1,
                observations_inserted: 1,
                observations_updated: 0,
            })
        );
        let stored_session: String = store
            .connection
            .query_row("SELECT session_id FROM sources", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_session, "current-session");
    }

    #[test]
    fn stale_imports_and_discoveries_do_not_regress_source_state() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let original = session_import("/sessions/a.jsonl", 10);
        store.commit_import(&original).unwrap();

        let mut newest = session_import("/sessions/a.jsonl", 99);
        newest.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_003_000);
        store.commit_import(&newest).unwrap();

        let mut stale = session_import("/sessions/a.jsonl", 50);
        stale.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
        assert_eq!(
            store.commit_import(&stale).unwrap(),
            CommitImportOutcome::IgnoredStale
        );

        store
            .record_discovery(
                &DiscoveryReport {
                    files: Vec::new(),
                    warnings: Vec::new(),
                    coverage: DiscoveryCoverage {
                        inspected_roots: vec![PathBuf::from("/sessions")],
                        inaccessible_paths: Vec::new(),
                    },
                },
                Timestamp::from_unix_milliseconds(1_700_000_005_000),
            )
            .unwrap();
        store
            .record_discovery(
                &DiscoveryReport {
                    files: vec![stale.source],
                    warnings: Vec::new(),
                    coverage: DiscoveryCoverage {
                        inspected_roots: vec![PathBuf::from("/sessions")],
                        inaccessible_paths: Vec::new(),
                    },
                },
                Timestamp::from_unix_milliseconds(1_700_000_004_000),
            )
            .unwrap();

        let states = store.source_states().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(
            states[0].last_successful_scan,
            Some(Timestamp::from_unix_milliseconds(1_700_000_003_000))
        );
        assert!(!states[0].present);
        let stored_tokens = store
            .connection
            .query_row("SELECT input_tokens FROM source_observations", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap();
        assert_eq!(decode_u64(&stored_tokens).unwrap(), 99);
    }

    #[test]
    fn reopening_a_database_preserves_imported_state() {
        let database = TempDatabase::new();
        {
            let mut store = SqliteUsageStore::open(&database.path).unwrap();
            store
                .commit_import(&session_import("/sessions/a.jsonl", u64::MAX))
                .unwrap();
        }

        let store = SqliteUsageStore::open(&database.path).unwrap();
        let states = store.source_states().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].path, PathBuf::from("/sessions/a.jsonl"));
        assert_eq!(states[0].last_imported_revision.as_ref().unwrap().size, 123);
        assert_eq!(
            states[0].last_successful_scan,
            Some(Timestamp::from_unix_milliseconds(1_700_000_001_000))
        );

        let stored_tokens = store
            .connection
            .query_row("SELECT input_tokens FROM source_observations", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .unwrap();
        assert_eq!(decode_u64(&stored_tokens).unwrap(), u64::MAX);
    }

    #[test]
    fn version_one_observations_do_not_fabricate_session_provenance() {
        let database = TempDatabase::new();
        let connection = Connection::open(&database.path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE sources (
                    id INTEGER PRIMARY KEY,
                    path BLOB NOT NULL UNIQUE,
                    agent TEXT,
                    session_id TEXT,
                    format_version INTEGER,
                    working_directory BLOB,
                    started_at_ms INTEGER,
                    name TEXT,
                    parent_session TEXT,
                    last_observed_size BLOB NOT NULL,
                    last_observed_modified_seconds INTEGER NOT NULL,
                    last_observed_modified_nanos INTEGER NOT NULL,
                    last_imported_size BLOB,
                    last_imported_modified_seconds INTEGER,
                    last_imported_modified_nanos INTEGER,
                    last_discovery_scan_ms INTEGER NOT NULL,
                    last_successful_scan_ms INTEGER,
                    last_parse_completion TEXT,
                    present INTEGER NOT NULL
                );
                CREATE TABLE usage_events (
                    id INTEGER PRIMARY KEY,
                    agent TEXT NOT NULL,
                    adapter_key TEXT NOT NULL,
                    UNIQUE (agent, adapter_key)
                );
                CREATE TABLE source_observations (
                    source_id INTEGER NOT NULL REFERENCES sources(id),
                    event_id INTEGER NOT NULL REFERENCES usage_events(id),
                    timestamp_ms INTEGER NOT NULL,
                    usage_kind TEXT NOT NULL,
                    provider TEXT,
                    model TEXT,
                    input_tokens BLOB NOT NULL,
                    output_tokens BLOB NOT NULL,
                    cache_read_tokens BLOB NOT NULL,
                    cache_write_tokens BLOB NOT NULL,
                    recorded_cost_usd REAL,
                    PRIMARY KEY (source_id, event_id)
                );
                CREATE INDEX source_observations_event
                    ON source_observations(event_id);
                PRAGMA user_version = 1;",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sources VALUES (
                    1, ?1, 'pi', 'replacement-session', 3, ?2, 1000, NULL, NULL,
                    ?3, 10, 20, ?3, 10, 20, 2000, 2000, 'complete', 1
                )",
                params![
                    encode_path(Path::new("/sessions/old.jsonl")),
                    encode_path(Path::new("/work/project")),
                    encode_u64(123),
                ],
            )
            .unwrap();
        connection
            .execute_batch(
                "INSERT INTO usage_events VALUES (1, 'pi', 'legacy-event');
                 INSERT INTO usage_events VALUES (2, 'pi', 'shared-event');",
            )
            .unwrap();
        for event_id in [1, 2] {
            connection
                .execute(
                    "INSERT INTO source_observations VALUES (
                        1, ?1, 1500, 'assistant', 'provider', 'model',
                        ?2, ?3, ?4, ?5, 0.25
                    )",
                    params![
                        event_id,
                        encode_u64(10),
                        encode_u64(2),
                        encode_u64(3),
                        encode_u64(4),
                    ],
                )
                .unwrap();
        }
        drop(connection);

        let mut store = SqliteUsageStore::open(&database.path).unwrap();
        let version: i64 = store
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let migrated_state = &store.source_states().unwrap()[0];
        assert!(migrated_state.last_imported_revision.is_some());
        assert!(migrated_state.reimport_required);

        let mut current = session_import("/sessions/old.jsonl", 99);
        current.parsed.metadata.session_id = "replacement-session".into();
        assert_eq!(
            store.commit_import(&current).unwrap(),
            CommitImportOutcome::Applied(ImportStats {
                event_identities_inserted: 0,
                observations_inserted: 0,
                observations_updated: 1,
            })
        );

        let mut statement = store
            .connection
            .prepare(
                "SELECT event.adapter_key, observation.session_provenance_known
                   FROM source_observations observation
                   JOIN usage_events event ON event.id = observation.event_id
                  ORDER BY event.adapter_key",
            )
            .unwrap();
        let provenance = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            provenance,
            vec![("legacy-event".into(), 0), ("shared-event".into(), 1)]
        );
        assert!(!store.source_states().unwrap()[0].reimport_required);
    }

    #[test]
    fn different_sources_retain_different_observations_for_one_event() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        store
            .commit_import(&session_import("/sessions/a.jsonl", 10))
            .unwrap();
        store
            .commit_import(&session_import("/sessions/b.jsonl", 99))
            .unwrap();

        let mut statement = store
            .connection
            .prepare(
                "SELECT o.input_tokens
                   FROM source_observations o
                   JOIN usage_events e ON e.id = o.event_id
                  WHERE e.agent = 'pi' AND e.adapter_key = 'shared-event'
                  ORDER BY o.input_tokens",
            )
            .unwrap();
        let values = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .unwrap()
            .map(|value| decode_u64(&value.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values, vec![10, 99]);
    }

    #[test]
    fn conflicting_observations_are_order_independent() {
        fn stored_observations(reverse: bool) -> Vec<(PathBuf, String, u64)> {
            let mut store = SqliteUsageStore::open_in_memory().unwrap();
            let first = session_import("/sessions/a.jsonl", 10);
            let mut second = session_import("/sessions/b.jsonl", 99);
            second.parsed.metadata.parent_session = Some("/sessions/a.jsonl".into());
            let imports = if reverse {
                [&second, &first]
            } else {
                [&first, &second]
            };
            for import in imports {
                store.commit_import(import).unwrap();
            }

            let mut statement = store
                .connection
                .prepare(
                    "SELECT source.path, session.session_id, observation.input_tokens
                       FROM source_observations observation
                       JOIN sources source ON source.id = observation.source_id
                       JOIN source_sessions session
                         ON session.id = observation.source_session_id
                      ORDER BY source.path",
                )
                .unwrap();
            statement
                .query_map([], |row| {
                    let path = decode_path(row.get(0)?).map_err(to_sql_conversion_error)?;
                    let tokens =
                        decode_u64(&row.get::<_, Vec<u8>>(2)?).map_err(to_sql_conversion_error)?;
                    Ok((path, row.get(1)?, tokens))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        }

        assert_eq!(stored_observations(false), stored_observations(true));
    }

    #[test]
    fn a_replaced_source_keeps_the_session_provenance_of_absent_observations() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        let mut original = session_import("/sessions/a.jsonl", 10);
        original.parsed.metadata.session_id = "original-session".into();
        store.commit_import(&original).unwrap();

        let mut replacement = session_import("/sessions/a.jsonl", 99);
        replacement.parsed.metadata.session_id = "replacement-session".into();
        replacement.parsed.events[0].identity.adapter_key = "replacement-event".into();
        replacement.scanned_at = Timestamp::from_unix_milliseconds(1_700_000_002_000);
        store.commit_import(&replacement).unwrap();

        let mut statement = store
            .connection
            .prepare(
                "SELECT event.adapter_key, session.session_id
                   FROM source_observations observation
                   JOIN usage_events event ON event.id = observation.event_id
                   JOIN source_sessions session
                     ON session.id = observation.source_session_id
                  ORDER BY event.adapter_key",
            )
            .unwrap();
        let provenance = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(
            provenance,
            vec![
                ("replacement-event".into(), "replacement-session".into()),
                ("shared-event".into(), "original-session".into()),
            ]
        );
    }

    #[test]
    fn a_missing_source_keeps_its_import_and_observations() {
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        store
            .commit_import(&session_import("/sessions/a.jsonl", 10))
            .unwrap();

        store
            .record_discovery(
                &DiscoveryReport {
                    files: Vec::new(),
                    warnings: Vec::new(),
                    coverage: DiscoveryCoverage {
                        inspected_roots: vec![PathBuf::from("/sessions")],
                        inaccessible_paths: Vec::new(),
                    },
                },
                Timestamp::from_unix_milliseconds(1_700_000_002_000),
            )
            .unwrap();

        let states = store.source_states().unwrap();
        assert_eq!(states.len(), 1);
        assert!(!states[0].present);
        assert!(states[0].last_imported_revision.is_some());
        let observations: i64 = store
            .connection
            .query_row("SELECT count(*) FROM source_observations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(observations, 1);
    }

    #[test]
    fn pre_epoch_file_times_round_trip() {
        let time = UNIX_EPOCH - Duration::new(1, 250_000_000);
        let (seconds, nanos) = system_time_to_parts(time).unwrap();
        assert_eq!((seconds, nanos), (-2, 750_000_000));
        assert_eq!(system_time_from_parts(seconds, nanos).unwrap(), time);
    }
}
