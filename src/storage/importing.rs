use super::{
    SqliteStoreError, SqliteUsageStore,
    codec::{
        attribution_parts, completion_to_str, encode_parent, encode_parse_notices, encode_path,
        encode_pricing_context, encode_u64, source_state_from_row, usage_kind_to_str,
    },
};
use crate::application::{
    CommitImportOutcome, DiscoveryReport, ImportStats, ObservationRetention, SessionImport,
    SnapshotCompletion, SourceState, UsageStore, ValidatedSessionImport,
};
use crate::domain::{AgentId, RecordedCost, Timestamp, UsageEvent};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::collections::HashSet;

impl UsageStore for SqliteUsageStore {
    type Error = SqliteStoreError;

    fn source_states(&self, agent: &AgentId) -> Result<Vec<SourceState>, Self::Error> {
        let mut statement = self.connection.prepare(
            "SELECT source_key, path, last_observed_revision, last_imported_revision,
                    last_successful_scan_ms, last_parse_completion, present, parse_notices, normalization_version
               FROM import_sources
              WHERE agent = ?1
              ORDER BY source_key",
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
        let discovered_keys: HashSet<_> = report.sources.iter().map(|source| &source.key).collect();
        for source in &report.sources {
            transaction
                .prepare_cached(
                    "INSERT INTO import_sources (
                    source_key, path, agent, last_observed_revision, last_discovery_scan_ms, present
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1)
                 ON CONFLICT(agent, source_key) DO UPDATE SET
                    path = excluded.path,
                    last_observed_revision = excluded.last_observed_revision,
                    last_discovery_scan_ms = excluded.last_discovery_scan_ms,
                    present = 1
                 WHERE excluded.last_discovery_scan_ms >= import_sources.last_discovery_scan_ms",
                )?
                .execute(params![
                    source.key.0,
                    source.path.as_deref().map(encode_path),
                    agent.as_str(),
                    source.revision.0,
                    observed_at.as_unix_milliseconds()
                ])?;
        }

        for key in &report.missing_sources {
            if discovered_keys.contains(key) {
                continue;
            }
            transaction
                .prepare_cached(
                    "UPDATE import_sources SET present = 0, last_discovery_scan_ms = ?1
                 WHERE agent = ?2 AND source_key = ?3 AND last_discovery_scan_ms <= ?1",
                )?
                .execute(params![
                    observed_at.as_unix_milliseconds(),
                    agent.as_str(),
                    key.0
                ])?;
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

        if import.session.completion != SnapshotCompletion::Complete
            && (import.session.observation_retention
                == ObservationRetention::ReplaceSessionObservations
                || normalization_changed(&transaction, import)?)
        {
            return Ok(CommitImportOutcome::DeferredIncomplete);
        }

        let source_id = upsert_imported_source(&transaction, import)?;
        let source_session_id = upsert_source_session(&transaction, source_id, import)?;
        let mut stats = ImportStats::default();
        let mut retained_events = HashSet::new();

        for event in &import.session.events {
            stats.event_identities_inserted += transaction
                .prepare_cached(
                    "INSERT INTO usage_events (agent, adapter_key)
                 VALUES (?1, ?2)
                 ON CONFLICT(agent, adapter_key) DO NOTHING",
                )?
                .execute(params![
                    event.identity.agent.as_str(),
                    &event.identity.adapter_key
                ])? as u64;

            let event_id: i64 = transaction
                .prepare_cached(
                    "SELECT id FROM usage_events WHERE agent = ?1 AND adapter_key = ?2",
                )?
                .query_row(
                    params![event.identity.agent.as_str(), &event.identity.adapter_key],
                    |row| row.get(0),
                )?;

            let pricing_context = encode_pricing_context(event.pricing_context.as_ref())?;
            retained_events.insert(event_id);
            let inserted = insert_observation(
                &transaction,
                source_id,
                source_session_id,
                event_id,
                event,
                pricing_context.as_deref(),
            )?;
            if inserted {
                stats.observations_inserted += 1;
            } else {
                stats.observations_updated += update_observation(
                    &transaction,
                    source_id,
                    source_session_id,
                    event_id,
                    event,
                    pricing_context.as_deref(),
                )? as u64;
            }
        }

        if import.session.observation_retention == ObservationRetention::ReplaceSessionObservations
        {
            remove_omitted_observations(&transaction, source_session_id, &retained_events)?;
        }

        transaction.commit()?;
        Ok(CommitImportOutcome::Applied(stats))
    }
}

fn remove_omitted_observations(
    transaction: &Transaction<'_>,
    source_session_id: i64,
    retained: &HashSet<i64>,
) -> Result<(), SqliteStoreError> {
    let mut statement = transaction
        .prepare_cached("SELECT event_id FROM usage_observations WHERE source_session_id = ?1")?;
    let ids = statement
        .query_map([source_session_id], |row| row.get::<_, i64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for id in ids {
        if !retained.contains(&id) {
            transaction
                .prepare_cached(
                    "DELETE FROM usage_observations WHERE source_session_id = ?1 AND event_id = ?2",
                )?
                .execute(params![source_session_id, id])?;
        }
    }
    Ok(())
}

fn normalization_changed(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<bool, SqliteStoreError> {
    transaction
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM import_sources
             WHERE agent = ?1 AND source_key = ?2 AND last_successful_scan_ms IS NOT NULL
               AND normalization_version IS NOT ?3)",
        )?
        .query_row(
            params![
                import.session.metadata.agent.as_str(),
                import.source.key.0,
                import.normalization_version.get()
            ],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn upsert_imported_source(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<i64, SqliteStoreError> {
    transaction
        .prepare_cached(
            "INSERT INTO import_sources (
            source_key, path, agent, last_observed_revision, last_imported_revision,
            last_discovery_scan_ms, last_successful_scan_ms, last_parse_completion,
            present, parse_notices, normalization_version
         ) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?5, ?6, 1, ?7, ?8)
         ON CONFLICT(agent, source_key) DO UPDATE SET
            path = excluded.path,
            last_observed_revision = excluded.last_observed_revision,
            last_imported_revision = excluded.last_imported_revision,
            last_discovery_scan_ms = excluded.last_discovery_scan_ms,
            last_successful_scan_ms = excluded.last_successful_scan_ms,
            last_parse_completion = excluded.last_parse_completion,
            parse_notices = excluded.parse_notices,
            normalization_version = excluded.normalization_version,
            present = 1
         RETURNING id",
        )?
        .query_row(
            params![
                import.source.key.0,
                import.source.path.as_deref().map(encode_path),
                import.session.metadata.agent.as_str(),
                import.source.revision.0,
                import.scanned_at.as_unix_milliseconds(),
                completion_to_str(import.session.completion),
                encode_parse_notices(&import.session.notices)?,
                import.normalization_version.get()
            ],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn upsert_source_session(
    transaction: &Transaction<'_>,
    source_id: i64,
    import: &SessionImport,
) -> Result<i64, SqliteStoreError> {
    let metadata = &import.session.metadata;
    let (parent_kind, parent_value) = encode_parent(metadata.parent_session.as_ref());
    transaction.prepare_cached(
        "INSERT INTO sessions (
            source_id, agent, session_id, working_directory, started_at_ms, name, parent_session, parent_kind
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(source_id, agent, session_id) DO UPDATE SET
            working_directory = excluded.working_directory,
            started_at_ms = excluded.started_at_ms,
            name = excluded.name,
            parent_session = excluded.parent_session,
            parent_kind = excluded.parent_kind
         RETURNING id",
    )?.query_row(
        params![source_id, metadata.agent.as_str(), metadata.session_id,
            metadata.working_directory.as_deref().map(encode_path), metadata.started_at.as_unix_milliseconds(),
            metadata.name, parent_value, parent_kind],
        |row| row.get(0),
    ).map_err(Into::into)
}

fn import_is_stale(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<bool, SqliteStoreError> {
    let stored = transaction.prepare_cached(
        "SELECT last_successful_scan_ms, last_discovery_scan_ms FROM import_sources WHERE source_key = ?1 AND agent = ?2",
    )?.query_row(
        params![import.source.key.0, import.session.metadata.agent.as_str()],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
    ).optional()?;
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
    pricing_context: Option<&str>,
) -> Result<bool, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    let inserted = transaction
        .prepare_cached(
            "INSERT INTO usage_observations (
            source_id, source_session_id, event_id, timestamp_ms, usage_kind, provider, model,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, recorded_cost_usd, pricing_context
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(source_session_id, event_id) DO NOTHING",
        )?
        .execute(params![
            source_id,
            source_session_id,
            event_id,
            event.timestamp.as_unix_milliseconds(),
            usage_kind_to_str(event.kind),
            provider,
            model,
            encode_u64(event.tokens.input)?,
            encode_u64(event.tokens.output)?,
            encode_u64(event.tokens.cache_read)?,
            encode_u64(event.tokens.cache_write)?,
            event.recorded_cost.map(RecordedCost::as_usd),
            pricing_context
        ])?
        == 1;
    Ok(inserted)
}

fn update_observation(
    transaction: &Transaction<'_>,
    source_id: i64,
    source_session_id: i64,
    event_id: i64,
    event: &UsageEvent,
    pricing_context: Option<&str>,
) -> Result<usize, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    transaction.prepare_cached(
        "UPDATE usage_observations SET timestamp_ms = ?1, usage_kind = ?2, provider = ?3, model = ?4,
            input_tokens = ?5, output_tokens = ?6, cache_read_tokens = ?7, cache_write_tokens = ?8,
            recorded_cost_usd = ?9, pricing_context = ?10
         WHERE source_id = ?11 AND source_session_id = ?12 AND event_id = ?13
           AND (timestamp_ms IS NOT ?1 OR usage_kind IS NOT ?2 OR provider IS NOT ?3 OR model IS NOT ?4
                OR input_tokens IS NOT ?5 OR output_tokens IS NOT ?6 OR cache_read_tokens IS NOT ?7
                OR cache_write_tokens IS NOT ?8 OR recorded_cost_usd IS NOT ?9 OR pricing_context IS NOT ?10)",
    )?.execute(
        params![event.timestamp.as_unix_milliseconds(), usage_kind_to_str(event.kind), provider, model,
            encode_u64(event.tokens.input)?, encode_u64(event.tokens.output)?, encode_u64(event.tokens.cache_read)?,
            encode_u64(event.tokens.cache_write)?, event.recorded_cost.map(RecordedCost::as_usd), pricing_context,
            source_id, source_session_id, event_id],
    ).map_err(Into::into)
}
