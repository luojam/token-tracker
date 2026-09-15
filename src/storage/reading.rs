use super::{
    SqliteStoreError, SqliteUsageStore,
    codec::{
        decode_parent, decode_parse_notices, decode_path, decode_pricing_context, decode_u64,
        from_sql_conversion_error, usage_kind_from_str,
    },
};
use crate::application::{
    ReportDiagnostic, SessionProvenance, SourceKey, SourceSessionKey, UsageObservation,
    UsageReadStore, UsageSnapshot,
};
use crate::domain::{
    AgentId, ModelAttribution, RecordedCost, Timestamp, TokenCounts, UsageEvent, UsageEventIdentity,
};
use rusqlite::Connection;
use std::collections::HashMap;

impl UsageReadStore for SqliteUsageStore {
    type Error = SqliteStoreError;

    fn usage_snapshot(&self) -> Result<UsageSnapshot, Self::Error> {
        let transaction = self.connection.unchecked_transaction()?;
        let sessions = load_stored_sessions(&transaction)?;
        let observations = load_stored_observations(&transaction, &sessions)?;
        let diagnostics = load_stored_diagnostics(&transaction)?;
        transaction.commit()?;

        let mut sessions = sessions.into_values().collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.key.cmp(&right.key));
        Ok(UsageSnapshot {
            sessions,
            observations,
            diagnostics,
        })
    }
}

fn load_stored_diagnostics(
    connection: &Connection,
) -> Result<Vec<ReportDiagnostic>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT agent, path, parse_notices FROM import_sources
         WHERE last_imported_revision IS NOT NULL
         ORDER BY agent, source_key",
    )?;
    let mut rows = statement.query([])?;
    let mut diagnostics = Vec::new();
    while let Some(row) = rows.next()? {
        let agent = AgentId::new(row.get::<_, String>(0)?);
        let path = row.get::<_, Option<Vec<u8>>>(1)?.map(decode_path);
        let notices = decode_parse_notices(&row.get::<_, String>(2)?)?;
        diagnostics.extend(notices.into_iter().map(|notice| ReportDiagnostic {
            agent: agent.clone(),
            path: path.clone(),
            notice,
        }));
    }
    Ok(diagnostics)
}

fn load_stored_sessions(
    connection: &Connection,
) -> Result<HashMap<i64, SessionProvenance>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT session.id, session.agent, session.session_id,
                session.started_at_ms, session.parent_session, source.source_key, session.parent_kind, source.path,
                session.name, session.working_directory
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
                name: row.get(8)?,
                working_directory: row.get::<_, Option<Vec<u8>>>(9)?.map(decode_path),
                started_at: Timestamp::from_unix_milliseconds(row.get(3)?),
                parent_session: decode_parent(row.get(6)?, row.get(4)?)
                    .map_err(from_sql_conversion_error)?,
            },
        ))
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

fn load_stored_observations(
    connection: &Connection,
    sessions: &HashMap<i64, SessionProvenance>,
) -> Result<Vec<UsageObservation>, SqliteStoreError> {
    let mut statement = connection.prepare(
        "SELECT event.agent, event.adapter_key, observation.source_session_id, observation.usage_kind,
                observation.provider, observation.model, observation.input_tokens, observation.output_tokens,
                observation.cache_read_tokens, observation.cache_write_tokens, observation.recorded_cost_usd,
                observation.timestamp_ms, observation.pricing_context
         FROM usage_observations observation
         JOIN usage_events event ON event.id = observation.event_id")?;
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
                pricing_context: decode_pricing_context(row.get(12)?, tokens)?,
            },
        });
    }

    Ok(observations)
}
