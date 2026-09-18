use std::{collections::HashSet, io, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::domain::export::EXPORT_FORMAT_VERSION;
use crate::{ExportSink, ExportSnapshot, PublishError, PublishOutcome};

const APPLICATION_ID: i32 = 0x54544558;

pub struct SqliteExportSink {
    connection: Connection,
}

impl SqliteExportSink {
    /// Opens an export database or initializes an empty database.
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        Self::from_connection(Connection::open(path)?, true)
    }

    /// Opens only an existing, identified export database.
    pub fn open_existing(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        Self::from_connection(
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?,
            false,
        )
    }

    fn from_connection(mut connection: Connection, initialize: bool) -> rusqlite::Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        {
            let transaction = connection.transaction()?;
            validate_destination(&transaction, initialize)?;
            transaction.commit()?;
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_destination(&transaction, initialize)?;
        transaction.execute_batch(include_str!("export_schema.sql"))?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.commit()?;
        Ok(Self { connection })
    }
}

fn validate_destination(connection: &Connection, initialize: bool) -> rusqlite::Result<()> {
    let application_id: i32 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let empty: bool = connection.query_row(
        "SELECT NOT EXISTS (SELECT 1 FROM sqlite_master)",
        [],
        |row| row.get(0),
    )?;
    if application_id != APPLICATION_ID
        && !(initialize && application_id == 0 && version == 0 && empty)
    {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "destination is not a token-tracker export database",
            ),
        )));
    }
    Ok(())
}

impl ExportSink for SqliteExportSink {
    type Error = rusqlite::Error;

    fn publish(
        &mut self,
        snapshot: &ExportSnapshot,
    ) -> Result<PublishOutcome, PublishError<Self::Error>> {
        let mut identities = HashSet::new();
        if snapshot.format_version != EXPORT_FORMAT_VERSION
            || snapshot.export_revision == 0
            || snapshot.machine_id.is_empty()
            || snapshot.events.iter().any(|event| {
                event.agent.is_empty()
                    || event.event_key.is_empty()
                    || !identities.insert((&event.agent, &event.event_key))
                    || [
                        event.tokens.input,
                        event.tokens.output,
                        event.tokens.cache_read,
                        event.tokens.cache_write,
                    ]
                    .into_iter()
                    .any(|count| count > i64::MAX as u64)
            })
        {
            return Err(PublishError::InvalidSnapshot {
                reason: "unsupported format, invalid identity, revision or token count, or duplicate event"
                    .into(),
            });
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(PublishError::Destination)?;
        let stored: Option<String> = transaction
            .query_row(
                "SELECT payload FROM snapshot WHERE machine_id = ?1",
                [&snapshot.machine_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(PublishError::Destination)?;
        if let Some(stored) = stored {
            let current: ExportSnapshot = serde_json::from_str(&stored).map_err(|error| {
                PublishError::Destination(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                ))
            })?;
            if snapshot.export_revision < current.export_revision {
                return Err(PublishError::StaleRevision {
                    incoming_revision: snapshot.export_revision,
                    published_revision: current.export_revision,
                });
            }
            if snapshot.export_revision == current.export_revision {
                return if snapshot == &current {
                    Ok(PublishOutcome::AlreadyPublished)
                } else {
                    Err(PublishError::RevisionConflict {
                        revision: snapshot.export_revision,
                    })
                };
            }
        }
        replace_machine_snapshot(&transaction, snapshot).map_err(PublishError::Destination)?;
        transaction.commit().map_err(PublishError::Destination)?;
        Ok(PublishOutcome::Published)
    }
}

fn replace_machine_snapshot(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: &ExportSnapshot,
) -> rusqlite::Result<()> {
    transaction.execute(
        "DELETE FROM events WHERE machine_id = ?1",
        [&snapshot.machine_id],
    )?;
    transaction.execute(
        "DELETE FROM snapshot WHERE machine_id = ?1",
        [&snapshot.machine_id],
    )?;
    transaction.execute(
        "INSERT INTO snapshot VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            snapshot.machine_id,
            snapshot.machine_name,
            snapshot.export_revision.to_string(),
            snapshot.format_version,
            snapshot.exported_at_unix_ms,
            json(snapshot)?,
        ],
    )?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO events VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )?;
        for event in &snapshot.events {
            let usage_kind = serde_json::to_value(event.usage_kind)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            insert.execute(params![
                snapshot.machine_id,
                event.agent,
                event.event_key,
                event.timestamp_unix_ms,
                usage_kind.as_str(),
                event.provider,
                event.model,
                event.tokens.input,
                event.tokens.output,
                event.tokens.cache_read,
                event.tokens.cache_write,
                event.recorded_cost_usd.as_ref().map(|cost| cost.as_str()),
                json(&event.estimate)?,
                event.pricing_context.as_ref().map(json).transpose()?,
                json(&event.sessions)?,
            ])?;
        }
    }
    Ok(())
}

fn json(value: &impl serde::Serialize) -> rusqlite::Result<String> {
    serde_json::to_string(value)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::EstimatedCost;

    #[test]
    fn preserves_integer_tokens_and_decimal_money_precision() {
        let mut snapshot: ExportSnapshot =
            serde_json::from_str(include_str!("../../tests/fixtures/export-example.json")).unwrap();
        snapshot.export_revision = u64::MAX;
        snapshot.events[0].tokens.input = i64::MAX as u64;
        snapshot.events[0].recorded_cost_usd =
            Some(EstimatedCost::from_picodollars(u128::MAX).into());
        let mut connection = Connection::open_in_memory().unwrap();
        let transaction = connection.transaction().unwrap();
        transaction
            .execute_batch(include_str!("export_schema.sql"))
            .unwrap();
        replace_machine_snapshot(&transaction, &snapshot).unwrap();
        transaction.commit().unwrap();
        let values: (String, i64, String) = connection
            .query_row(
                "SELECT export_revision, input_tokens, recorded_cost_usd FROM snapshot, events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            values,
            (
                "18446744073709551615".into(),
                i64::MAX,
                "340282366920938463463374607.431768211455".into(),
            )
        );
    }
}
