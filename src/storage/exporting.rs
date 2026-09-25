use std::{io, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use super::SqliteStoreError;
use crate::{ExportSink, ExportSnapshot, PublishError, PublishOutcome};
use crate::{
    application::SummaryReadStore,
    domain::{
        ExportSummary, TokenCounts,
        export::{ExportEstimate, UsdAmount},
    },
};

const APPLICATION_ID: i32 = 0x54544558;

pub struct SqliteExportStore {
    connection: Connection,
}

impl SqliteExportStore {
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

impl SummaryReadStore for SqliteExportStore {
    type Error = SqliteStoreError;

    fn summary(&self) -> Result<ExportSummary, Self::Error> {
        let mut statement = self.connection.prepare(
            "SELECT input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    recorded_cost_usd, estimate FROM events",
        )?;
        let mut rows = statement.query([])?;
        let mut summary = ExportSummary::default();
        while let Some(row) = rows.next()? {
            let tokens = TokenCounts {
                input: row.get(0)?,
                output: row.get(1)?,
                cache_read: row.get(2)?,
                cache_write: row.get(3)?,
            };
            summary.tokens = summary
                .tokens
                .checked_add(tokens)
                .ok_or(SqliteStoreError::ValueOutOfRange("summary token total"))?;
            let cost = match row.get::<_, Option<String>>(4)? {
                Some(value) => {
                    Some(UsdAmount::try_from(value).map_err(SqliteStoreError::CorruptData)?)
                }
                None => match serde_json::from_str::<ExportEstimate>(&row.get::<_, String>(5)?)
                    .map_err(|_| SqliteStoreError::CorruptData("an invalid exported estimate"))?
                {
                    ExportEstimate::Available { cost_usd, .. } => Some(cost_usd),
                    ExportEstimate::Unavailable { .. } | ExportEstimate::NotNeeded => None,
                },
            };
            if let Some(cost) = cost {
                summary.total_cost_usd = summary.total_cost_usd.add(&cost);
            }
        }
        Ok(summary)
    }
}

impl ExportSink for SqliteExportStore {
    type Error = rusqlite::Error;

    fn publish(
        &mut self,
        snapshot: &ExportSnapshot,
    ) -> Result<PublishOutcome, PublishError<Self::Error>> {
        snapshot
            .validate()
            .map_err(|reason| PublishError::InvalidSnapshot {
                reason: reason.into(),
            })?;
        if snapshot.events.iter().any(|event| {
            [
                event.tokens.input,
                event.tokens.output,
                event.tokens.cache_read,
                event.tokens.cache_write,
            ]
            .into_iter()
            .any(|count| count > i64::MAX as u64)
        }) {
            return Err(PublishError::InvalidSnapshot {
                reason: "token count exceeds SQLite signed 64-bit integer range".into(),
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

    #[test]
    fn summary_rejects_token_overflow() {
        let mut snapshot: ExportSnapshot =
            serde_json::from_str(include_str!("../../tests/fixtures/export-example.json")).unwrap();
        snapshot.events[0].tokens.input = i64::MAX as u64;
        let mut sink = SqliteExportStore::open(":memory:").unwrap();
        for machine in ["first", "second", "third"] {
            snapshot.machine_id = machine.into();
            sink.publish(&snapshot).unwrap();
        }
        assert!(matches!(
            sink.summary(),
            Err(SqliteStoreError::ValueOutOfRange("summary token total"))
        ));
    }
}
