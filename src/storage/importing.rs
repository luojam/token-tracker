use super::{
    SqliteStoreError, attribution_parts, billing, completion_to_str, encode_parent, encode_path,
    encode_u64, parse_notices, system_time_to_parts, usage_kind_to_str,
};
use crate::application::SessionImport;
use crate::domain::{RecordedCost, UsageEvent};
use rusqlite::{OptionalExtension, Transaction, params};

pub(super) fn normalization_changed(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<bool, SqliteStoreError> {
    transaction
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM import_sources
             WHERE agent = ?1 AND path = ?2 AND last_successful_scan_ms IS NOT NULL
               AND normalization_version IS NOT ?3)",
            params![
                import.parsed.metadata.agent.as_str(),
                encode_path(&import.source.path),
                import.normalization_version
            ],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(super) fn upsert_imported_source(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<i64, SqliteStoreError> {
    let path = encode_path(&import.source.path);
    let (seconds, nanos) = system_time_to_parts(import.source.revision.modified_at)?;
    transaction.execute(
        "INSERT INTO import_sources (
            path, agent, last_observed_size, last_observed_modified_seconds,
            last_observed_modified_nanos, last_imported_size, last_imported_modified_seconds,
            last_imported_modified_nanos, last_discovery_scan_ms, last_successful_scan_ms,
            last_parse_completion, present, parse_notices, normalization_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?3, ?4, ?5, ?6, ?6, ?7, 1, ?8, ?9)
         ON CONFLICT(agent, path) DO UPDATE SET
            last_observed_size = excluded.last_observed_size,
            last_observed_modified_seconds = excluded.last_observed_modified_seconds,
            last_observed_modified_nanos = excluded.last_observed_modified_nanos,
            last_imported_size = excluded.last_imported_size,
            last_imported_modified_seconds = excluded.last_imported_modified_seconds,
            last_imported_modified_nanos = excluded.last_imported_modified_nanos,
            last_discovery_scan_ms = excluded.last_discovery_scan_ms,
            last_successful_scan_ms = excluded.last_successful_scan_ms,
            last_parse_completion = excluded.last_parse_completion,
            parse_notices = excluded.parse_notices,
            normalization_version = excluded.normalization_version,
            present = 1",
        params![
            path,
            import.parsed.metadata.agent.as_str(),
            encode_u64(import.source.revision.size)?,
            seconds,
            nanos,
            import.scanned_at.as_unix_milliseconds(),
            completion_to_str(import.parsed.completion),
            parse_notices::encode(&import.parsed.notices)?,
            import.normalization_version
        ],
    )?;
    transaction
        .query_row(
            "SELECT id FROM import_sources WHERE path = ?1 AND agent = ?2",
            params![path, import.parsed.metadata.agent.as_str()],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(super) fn upsert_source_session(
    transaction: &Transaction<'_>,
    source_id: i64,
    import: &SessionImport,
) -> Result<i64, SqliteStoreError> {
    let metadata = &import.parsed.metadata;
    let (parent_kind, parent_value) = encode_parent(metadata.parent_session.as_ref());
    transaction.execute(
        "INSERT INTO sessions (
            source_id, agent, session_id, working_directory, started_at_ms, name, parent_session, parent_kind
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(source_id, agent, session_id) DO UPDATE SET
            working_directory = excluded.working_directory,
            started_at_ms = excluded.started_at_ms,
            name = excluded.name,
            parent_session = excluded.parent_session,
            parent_kind = excluded.parent_kind",
        params![source_id, metadata.agent.as_str(), metadata.session_id,
            metadata.working_directory.as_deref().map(encode_path), metadata.started_at.as_unix_milliseconds(),
            metadata.name, parent_value, parent_kind],
    )?;
    transaction
        .query_row(
            "SELECT id FROM sessions WHERE source_id = ?1 AND agent = ?2 AND session_id = ?3",
            params![source_id, metadata.agent.as_str(), metadata.session_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

pub(super) fn import_is_stale(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<bool, SqliteStoreError> {
    let stored = transaction.query_row(
        "SELECT last_successful_scan_ms, last_discovery_scan_ms FROM import_sources WHERE path = ?1 AND agent = ?2",
        params![encode_path(&import.source.path), import.parsed.metadata.agent.as_str()],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
    ).optional()?;
    let Some((last_successful_scan, last_discovery_scan)) = stored else {
        return Ok(false);
    };
    let scanned_at = import.scanned_at.as_unix_milliseconds();
    Ok(last_discovery_scan > scanned_at
        || last_successful_scan.is_some_and(|last_scan| last_scan > scanned_at))
}

pub(super) fn insert_observation(
    transaction: &Transaction<'_>,
    source_id: i64,
    source_session_id: i64,
    event_id: i64,
    event: &UsageEvent,
) -> Result<bool, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    let inserted = transaction.execute(
        "INSERT INTO usage_observations (
            source_id, source_session_id, event_id, timestamp_ms, usage_kind, provider, model,
            input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, recorded_cost_usd
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(source_session_id, event_id) DO NOTHING",
        params![
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
            event.recorded_cost.map(RecordedCost::as_usd)
        ],
    )? == 1;
    if inserted {
        write_billing(transaction, source_session_id, event_id, event)?;
    }
    Ok(inserted)
}

pub(super) fn update_observation(
    transaction: &Transaction<'_>,
    source_id: i64,
    source_session_id: i64,
    event_id: i64,
    event: &UsageEvent,
) -> Result<usize, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    let changed = transaction.execute(
        "UPDATE usage_observations SET timestamp_ms = ?1, usage_kind = ?2, provider = ?3, model = ?4,
            input_tokens = ?5, output_tokens = ?6, cache_read_tokens = ?7, cache_write_tokens = ?8,
            recorded_cost_usd = ?9
         WHERE source_id = ?10 AND source_session_id = ?11 AND event_id = ?12
           AND (timestamp_ms IS NOT ?1 OR usage_kind IS NOT ?2 OR provider IS NOT ?3 OR model IS NOT ?4
                OR input_tokens IS NOT ?5 OR output_tokens IS NOT ?6 OR cache_read_tokens IS NOT ?7
                OR cache_write_tokens IS NOT ?8 OR recorded_cost_usd IS NOT ?9)",
        params![event.timestamp.as_unix_milliseconds(), usage_kind_to_str(event.kind), provider, model,
            encode_u64(event.tokens.input)?, encode_u64(event.tokens.output)?, encode_u64(event.tokens.cache_read)?,
            encode_u64(event.tokens.cache_write)?, event.recorded_cost.map(RecordedCost::as_usd), source_id, source_session_id, event_id],
    )?;
    let billing_changed = write_billing(transaction, source_session_id, event_id, event)?;
    Ok(usize::from(changed > 0 || billing_changed))
}

fn write_billing(
    transaction: &Transaction<'_>,
    session: i64,
    event: i64,
    usage: &UsageEvent,
) -> Result<bool, SqliteStoreError> {
    let changed = match billing::encode(usage.pricing_context.as_ref())? {
        Some(facts) => transaction.execute(
            "INSERT INTO billing_inputs (source_session_id, event_id, facts) VALUES (?1, ?2, ?3)
             ON CONFLICT(source_session_id, event_id) DO UPDATE SET facts = excluded.facts
             WHERE facts IS NOT excluded.facts",
            params![session, event, facts],
        )?,
        None => transaction.execute(
            "DELETE FROM billing_inputs WHERE source_session_id = ?1 AND event_id = ?2",
            params![session, event],
        )?,
    };
    Ok(changed > 0)
}

pub(super) fn validate_import(import: &SessionImport) -> Result<(), SqliteStoreError> {
    if import.normalization_version == 0 {
        return Err(SqliteStoreError::InvalidImport(
            "normalization version must be positive",
        ));
    }
    for event in &import.parsed.events {
        if event
            .pricing_context
            .as_ref()
            .is_some_and(|context| !context.usage_matches(event.tokens))
        {
            return Err(SqliteStoreError::InvalidImport(
                "invalid billing usage components or totals",
            ));
        }
        if event.identity.agent != import.parsed.metadata.agent {
            return Err(SqliteStoreError::InvalidImport(
                "an event agent does not match its session agent",
            ));
        }
    }
    Ok(())
}
