use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use token_tracker::ExportSnapshot;

use super::CliError;

const APPLICATION_ID: i32 = 0x54544558;

pub(super) fn write_snapshot(
    path: &Path,
    force: bool,
    snapshot: &ExportSnapshot,
) -> Result<(), CliError> {
    let output_error = |source| CliError::ExportOutput {
        path: path.to_owned(),
        source,
    };
    let path = std::path::absolute(path).map_err(output_error)?;
    let created = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(_) => true,
        Err(error) if force && error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(output_error(error)),
    };
    let result = write_database(&path, created, snapshot);
    if result.is_err() && created {
        let _ = fs::remove_file(&path);
    }
    result.map_err(|source| CliError::ExportDatabase { path, source })
}

fn write_database(path: &Path, created: bool, snapshot: &ExportSnapshot) -> rusqlite::Result<()> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let application_id: i32 =
        transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if !created && application_id != APPLICATION_ID {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            io::Error::new(
                io::ErrorKind::InvalidData,
                "destination is not a token-tracker export database",
            ),
        )));
    }
    replace_contents(&transaction, snapshot)?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.commit()
}

fn replace_contents(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: &ExportSnapshot,
) -> rusqlite::Result<()> {
    transaction.execute_batch(include_str!("export_schema.sql"))?;
    transaction.execute("DELETE FROM events", [])?;
    transaction.execute("DELETE FROM snapshot", [])?;
    transaction.execute(
        "INSERT INTO snapshot VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![
            snapshot.machine_id,
            snapshot.machine_name,
            snapshot.export_revision.to_string(),
            snapshot.format_version,
            snapshot.exported_at_unix_ms,
        ],
    )?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO events VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        for event in &snapshot.events {
            let usage_kind = serde_json::to_value(event.usage_kind)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            insert.execute(params![
                event.agent,
                event.event_key,
                event.timestamp_unix_ms,
                usage_kind.as_str(),
                event.provider,
                event.model,
                event.tokens.input.to_string(),
                event.tokens.output.to_string(),
                event.tokens.cache_read.to_string(),
                event.tokens.cache_write.to_string(),
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
    use token_tracker::domain::EstimatedCost;

    #[test]
    fn preserves_unsigned_integer_and_decimal_money_precision() {
        let mut snapshot: ExportSnapshot =
            serde_json::from_str(include_str!("../../tests/fixtures/export-example.json")).unwrap();
        snapshot.export_revision = u64::MAX;
        snapshot.events[0].tokens.input = u64::MAX;
        snapshot.events[0].recorded_cost_usd =
            Some(EstimatedCost::from_picodollars(u128::MAX).into());
        let mut connection = Connection::open_in_memory().unwrap();
        let transaction = connection.transaction().unwrap();
        replace_contents(&transaction, &snapshot).unwrap();
        transaction.commit().unwrap();
        let values: (String, String, String) = connection
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
                "18446744073709551615".into(),
                "340282366920938463463374607.431768211455".into(),
            )
        );
    }
}
