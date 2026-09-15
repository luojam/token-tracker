use super::SqliteStoreError;
use rusqlite::Connection;

const APPLICATION_ID: i32 = 0x54545553;
// Changes to schema.sql require recreating existing databases.
const SCHEMA_VERSION: i64 = 1;

pub(super) fn initialize(connection: &mut Connection) -> Result<(), SqliteStoreError> {
    let transaction = connection.transaction()?;
    let application_id: i32 =
        transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application_id == APPLICATION_ID {
        if version != SCHEMA_VERSION {
            return Err(SqliteStoreError::UnsupportedSchemaVersion(version));
        }
    } else {
        let empty: bool = transaction.query_row(
            "SELECT NOT EXISTS (SELECT 1 FROM sqlite_schema)",
            [],
            |row| row.get(0),
        )?;
        if application_id != 0 || version != 0 || !empty {
            return Err(SqliteStoreError::NotUsageDatabase);
        }
        transaction.execute_batch(include_str!("schema.sql"))?;
        transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    transaction.commit()?;
    Ok(())
}
