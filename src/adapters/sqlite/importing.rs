use super::{
    SqliteStoreError, attribution_parts, completion_to_str, encode_parent, encode_path, encode_u64,
    parse_notices, pricing_context, system_time_to_parts, usage_kind_to_str,
};
use crate::application::SessionImport;
use crate::core::{RecordedCost, UsageEvent};
use rusqlite::{OptionalExtension, Transaction, params};

pub(super) fn upsert_imported_source(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<i64, SqliteStoreError> {
    let path = encode_path(&import.source.path);
    let working_directory = import
        .parsed
        .metadata
        .working_directory
        .as_deref()
        .map(encode_path);
    let (parent_kind, parent_value) = encode_parent(import.parsed.metadata.parent_session.as_ref());
    let (modified_seconds, modified_nanos) =
        system_time_to_parts(import.source.revision.modified_at)?;
    let size = encode_u64(import.source.revision.size);
    let notices = parse_notices::encode(&import.parsed.notices)?;

    transaction.execute(
        "INSERT INTO sources (
            path, agent, session_id, format_version, working_directory,
            started_at_ms, name, parent_session,
            last_observed_size, last_observed_modified_seconds,
            last_observed_modified_nanos,
            last_imported_size, last_imported_modified_seconds,
            last_imported_modified_nanos,
            last_discovery_scan_ms, last_successful_scan_ms,
            last_parse_completion, present, parent_kind, parse_notices
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?9, ?10, ?11, ?12, ?12, ?13, 1, ?14, ?15
         )
         ON CONFLICT(agent, path) DO UPDATE SET
            agent = excluded.agent,
            session_id = excluded.session_id,
            format_version = excluded.format_version,
            working_directory = excluded.working_directory,
            started_at_ms = excluded.started_at_ms,
            name = excluded.name,
            parent_session = excluded.parent_session,
            parent_kind = excluded.parent_kind,
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
            parse_notices = excluded.parse_notices,
            present = CASE
                WHEN excluded.last_discovery_scan_ms >= sources.last_discovery_scan_ms
                THEN 1 ELSE sources.present END",
        params![
            &path,
            import.parsed.metadata.agent.as_str(),
            &import.parsed.metadata.session_id,
            &import.parsed.metadata.format_version,
            working_directory,
            import.parsed.metadata.started_at.as_unix_milliseconds(),
            &import.parsed.metadata.name,
            parent_value,
            size,
            modified_seconds,
            modified_nanos,
            import.scanned_at.as_unix_milliseconds(),
            completion_to_str(import.parsed.completion),
            parent_kind,
            notices,
        ],
    )?;

    transaction
        .query_row(
            "SELECT id FROM sources WHERE path = ?1 AND agent = ?2",
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
        "INSERT INTO source_sessions (
            source_id, agent, session_id, format_version, working_directory,
            started_at_ms, name, parent_session, parent_kind
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(source_id, agent, session_id) DO UPDATE SET
            format_version = excluded.format_version,
            working_directory = excluded.working_directory,
            started_at_ms = excluded.started_at_ms,
            name = excluded.name,
            parent_session = excluded.parent_session,
            parent_kind = excluded.parent_kind",
        params![
            source_id,
            metadata.agent.as_str(),
            &metadata.session_id,
            &metadata.format_version,
            metadata.working_directory.as_deref().map(encode_path),
            metadata.started_at.as_unix_milliseconds(),
            &metadata.name,
            parent_value,
            parent_kind,
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

pub(super) fn import_is_stale(
    transaction: &Transaction<'_>,
    import: &SessionImport,
) -> Result<bool, SqliteStoreError> {
    let path = encode_path(&import.source.path);
    let stored = transaction
        .query_row(
            "SELECT last_successful_scan_ms, last_discovery_scan_ms
               FROM sources
              WHERE path = ?1 AND agent = ?2",
            params![path, import.parsed.metadata.agent.as_str()],
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

pub(super) fn insert_observation(
    transaction: &Transaction<'_>,
    source_id: i64,
    source_session_id: i64,
    event_id: i64,
    event: &UsageEvent,
) -> Result<bool, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    let pricing = pricing_context::encode(event.pricing_context.as_ref());
    let changed = transaction.execute(
        "INSERT INTO source_observations (
            source_id, source_session_id, event_id, timestamp_ms, usage_kind,
            provider, model, input_tokens, output_tokens, cache_read_tokens,
            cache_write_tokens, recorded_cost_usd,
            pricing_tier, pricing_unsupported_tier, pricing_raw_tier_kind,
            pricing_raw_tier_value, pricing_tier_evidence,
            pricing_request_granularity, pricing_cache_detail, pricing_request_usage,
            pricing_anthropic
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                   ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
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
            pricing[0],
            pricing[1],
            pricing[2],
            pricing[3],
            pricing[4],
            pricing[5],
            pricing[6],
            pricing_context::encode_requests(event.pricing_context.as_ref()),
            pricing_context::encode_anthropic(event.pricing_context.as_ref())?,
        ],
    )?;
    Ok(changed == 1)
}

pub(super) fn update_observation(
    transaction: &Transaction<'_>,
    source_id: i64,
    source_session_id: i64,
    event_id: i64,
    event: &UsageEvent,
) -> Result<usize, SqliteStoreError> {
    let (provider, model) = attribution_parts(event);
    let pricing = pricing_context::encode(event.pricing_context.as_ref());
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
                    pricing_tier = ?13,
                    pricing_unsupported_tier = ?14,
                    pricing_raw_tier_kind = ?15,
                    pricing_raw_tier_value = ?16,
                    pricing_tier_evidence = ?17,
                    pricing_request_granularity = ?18,
                    pricing_cache_detail = ?19,
                    pricing_request_usage = ?20,
                    pricing_anthropic = ?21
              WHERE source_id = ?10 AND source_session_id = ?11 AND event_id = ?12
                AND (timestamp_ms IS NOT ?1
                     OR usage_kind IS NOT ?2
                     OR provider IS NOT ?3
                     OR model IS NOT ?4
                     OR input_tokens IS NOT ?5
                     OR output_tokens IS NOT ?6
                     OR cache_read_tokens IS NOT ?7
                     OR cache_write_tokens IS NOT ?8
                     OR recorded_cost_usd IS NOT ?9
                     OR pricing_tier IS NOT ?13
                     OR pricing_unsupported_tier IS NOT ?14
                     OR pricing_raw_tier_kind IS NOT ?15
                     OR pricing_raw_tier_value IS NOT ?16
                     OR pricing_tier_evidence IS NOT ?17
                     OR pricing_request_granularity IS NOT ?18
                     OR pricing_cache_detail IS NOT ?19
                     OR pricing_request_usage IS NOT ?20
                     OR pricing_anthropic IS NOT ?21)",
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
                pricing[0],
                pricing[1],
                pricing[2],
                pricing[3],
                pricing[4],
                pricing[5],
                pricing[6],
                pricing_context::encode_requests(event.pricing_context.as_ref()),
                pricing_context::encode_anthropic(event.pricing_context.as_ref())?,
            ],
        )
        .map_err(Into::into)
}

pub(super) fn validate_import(import: &SessionImport) -> Result<(), SqliteStoreError> {
    if import.parsed.events.iter().any(|event| {
        event
            .pricing_context
            .as_ref()
            .is_some_and(|context| !context.usage_matches(event.tokens))
    }) {
        return Err(SqliteStoreError::InvalidImport(
            "invalid pricing usage components or totals",
        ));
    }
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
