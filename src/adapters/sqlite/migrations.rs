use super::SqliteStoreError;
use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[include_str!("schema_v1.sql")];
pub(super) const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

pub(super) fn migrate(connection: &mut Connection) -> Result<(), SqliteStoreError> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(SqliteStoreError::UnsupportedSchemaVersion(version));
    }
    for migration in &MIGRATIONS[version as usize..] {
        let transaction = connection.transaction()?;
        transaction.execute_batch(migration)?;
        transaction.commit()?;
    }
    Ok(())
}
