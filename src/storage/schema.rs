use super::SqliteStoreError;
use rusqlite::Connection;

// Changes to schema.sql require recreating existing databases.
const INITIAL_SCHEMA_VERSION: i64 = 1;
const MIGRATIONS: &[&str] = &[];
pub(super) const SCHEMA_VERSION: i64 = INITIAL_SCHEMA_VERSION + MIGRATIONS.len() as i64;

pub(super) fn migrate(connection: &mut Connection) -> Result<(), SqliteStoreError> {
    let transaction = connection.transaction()?;
    let mut version: i64 =
        transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 0 {
        transaction.execute_batch(include_str!("schema.sql"))?;
        version = INITIAL_SCHEMA_VERSION;
    } else if !(INITIAL_SCHEMA_VERSION..=SCHEMA_VERSION).contains(&version) {
        return Err(SqliteStoreError::UnsupportedSchemaVersion(version));
    }
    for migration in &MIGRATIONS[(version - INITIAL_SCHEMA_VERSION) as usize..] {
        transaction.execute_batch(migration)?;
    }
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}
