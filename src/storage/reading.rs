use super::{
    SqliteStoreError, billing, decode_parent, decode_path, decode_u64, to_sql_conversion_error,
    usage_kind_from_str,
};
use crate::application::{SessionProvenance, SourceKey, SourceSessionKey, UsageObservation};
use crate::domain::{
    AgentId, ModelAttribution, RecordedCost, Timestamp, TokenCounts, UsageEvent, UsageEventIdentity,
};
use rusqlite::Connection;
use std::collections::HashMap;

pub(super) fn load_stored_sessions(
    connection: &Connection,
) -> Result<HashMap<i64, SessionProvenance>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT session.id, session.agent, session.session_id,
                session.started_at_ms, session.parent_session, source.source_key, session.parent_kind, source.path
         FROM sessions session JOIN import_sources source ON source.id = session.source_id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get(0)?,
            SessionProvenance {
                key: SourceSessionKey {
                    agent: AgentId::new(row.get::<_, String>(1)?),
                    session_id: row.get(2)?,
                    source: SourceKey(row.get(5)?),
                },
                source_path: row.get::<_, Option<Vec<u8>>>(7)?.map(decode_path),
                started_at: Timestamp::from_unix_milliseconds(row.get(3)?),
                parent_session: decode_parent(row.get(6)?, row.get(4)?)
                    .map_err(to_sql_conversion_error)?,
            },
        ))
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

pub(super) fn load_stored_observations(
    connection: &Connection,
    sessions: &HashMap<i64, SessionProvenance>,
) -> Result<Vec<UsageObservation>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT event.agent, event.adapter_key, observation.source_session_id, observation.usage_kind,
                observation.provider, observation.model, observation.input_tokens, observation.output_tokens,
                observation.cache_read_tokens, observation.cache_write_tokens, observation.recorded_cost_usd,
                observation.timestamp_ms, billing.facts
         FROM usage_observations observation
         JOIN usage_events event ON event.id = observation.event_id
         LEFT JOIN billing_inputs billing ON billing.source_session_id = observation.source_session_id
                                         AND billing.event_id = observation.event_id")?;
    let mut rows = statement.query([])?;
    let mut observations = Vec::new();
    while let Some(row) = rows.next()? {
        let tokens = TokenCounts {
            input: decode_u64(row.get(6)?)?,
            output: decode_u64(row.get(7)?)?,
            cache_read: decode_u64(row.get(8)?)?,
            cache_write: decode_u64(row.get(9)?)?,
        };
        let attribution = match (
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ) {
            (Some(provider), Some(model)) => Some(ModelAttribution { provider, model }),
            (None, None) => None,
            _ => {
                return Err(SqliteStoreError::CorruptData(
                    "an incomplete model attribution",
                ));
            }
        };
        let recorded_cost = row
            .get::<_, Option<f64>>(10)?
            .map(RecordedCost::from_usd)
            .transpose()
            .map_err(|_| SqliteStoreError::CorruptData("an invalid recorded cost"))?;
        let session = sessions
            .get(&row.get::<_, i64>(2)?)
            .ok_or(SqliteStoreError::CorruptData(
                "an observation without session provenance",
            ))?
            .key
            .clone();
        observations.push(UsageObservation {
            session,
            event: UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::new(row.get::<_, String>(0)?),
                    adapter_key: row.get(1)?,
                },
                timestamp: Timestamp::from_unix_milliseconds(row.get(11)?),
                kind: usage_kind_from_str(&row.get::<_, String>(3)?)?,
                attribution,
                tokens,
                recorded_cost,
                pricing_context: billing::decode(row.get(12)?, tokens)?,
            },
        });
    }
    Ok(observations)
}
