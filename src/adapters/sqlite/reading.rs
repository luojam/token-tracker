use super::{
    SqliteStoreError, decode_parent, decode_path, decode_u64, pricing_context,
    to_sql_conversion_error, usage_kind_from_str,
};
use crate::application::{SessionProvenance, SourceSessionKey, UsageObservation};
use crate::core::{
    AgentId, ModelAttribution, RecordedCost, Timestamp, TokenCounts, UsageEvent, UsageEventIdentity,
};
use rusqlite::Connection;
use std::collections::HashMap;

pub(super) fn load_stored_sessions(
    connection: &Connection,
) -> Result<HashMap<i64, SessionProvenance>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT session.id, session.agent, session.session_id,
                session.started_at_ms, session.parent_session, source.path, session.parent_kind
           FROM source_sessions session
           JOIN sources source ON source.id = session.source_id",
    )?;
    let rows = statement.query_map([], |row| {
        let parent = decode_parent(row.get(6)?, row.get(4)?).map_err(to_sql_conversion_error)?;
        Ok((
            row.get(0)?,
            SessionProvenance {
                key: SourceSessionKey {
                    agent: AgentId::new(row.get::<_, String>(1)?),
                    session_id: row.get(2)?,
                    source_path: decode_path(row.get(5)?),
                },
                started_at: Timestamp::from_unix_milliseconds(row.get(3)?),
                parent_session: parent,
            },
        ))
    })?;
    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(Into::into)
}

pub(super) fn load_stored_observations(
    connection: &Connection,
    sessions: &HashMap<i64, SessionProvenance>,
) -> Result<Vec<UsageObservation>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT event.agent, event.adapter_key, observation.source_session_id,
                observation.usage_kind,
                observation.provider, observation.model,
                observation.input_tokens, observation.output_tokens,
                observation.cache_read_tokens, observation.cache_write_tokens,
                observation.recorded_cost_usd, observation.timestamp_ms,
                observation.pricing_tier, observation.pricing_unsupported_tier,
                observation.pricing_raw_tier_kind, observation.pricing_raw_tier_value,
                observation.pricing_tier_evidence, observation.pricing_request_granularity,
                observation.pricing_cache_detail, observation.pricing_request_usage
           FROM source_observations observation
           JOIN usage_events event ON event.id = observation.event_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Vec<u8>>(6)?,
            row.get::<_, Vec<u8>>(7)?,
            row.get::<_, Vec<u8>>(8)?,
            row.get::<_, Vec<u8>>(9)?,
            row.get::<_, Option<f64>>(10)?,
            row.get::<_, i64>(11)?,
            [
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, Option<String>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<String>>(16)?,
                row.get::<_, Option<String>>(17)?,
                row.get::<_, Option<String>>(18)?,
            ],
            row.get::<_, Option<Vec<u8>>>(19)?,
        ))
    })?;

    let mut observations = Vec::new();
    for row in rows {
        let (
            agent,
            adapter_key,
            source_session_id,
            kind,
            provider,
            model,
            input,
            output,
            cache_read,
            cache_write,
            recorded_cost,
            timestamp_ms,
            pricing,
            request_usage,
        ) = row?;
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

        let session = sessions
            .get(&source_session_id)
            .ok_or(SqliteStoreError::CorruptData(
                "an observation without session provenance",
            ))?
            .key
            .clone();
        let tokens = TokenCounts {
            input: decode_u64(&input)?,
            output: decode_u64(&output)?,
            cache_read: decode_u64(&cache_read)?,
            cache_write: decode_u64(&cache_write)?,
        };
        let pricing_context = pricing_context::decode(pricing, request_usage)?;
        if pricing_context
            .as_ref()
            .is_some_and(|context| !context.request_usage_matches(tokens))
        {
            return Err(SqliteStoreError::CorruptData(
                "a request usage breakdown that differs from its total",
            ));
        }
        observations.push(UsageObservation {
            session,
            event: UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::new(agent),
                    adapter_key,
                },
                timestamp: Timestamp::from_unix_milliseconds(timestamp_ms),
                kind: usage_kind_from_str(&kind)?,
                attribution,
                tokens,
                recorded_cost,
                pricing_context,
            },
        });
    }
    Ok(observations)
}
