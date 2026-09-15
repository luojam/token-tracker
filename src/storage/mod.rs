mod codec;
mod error;
mod exporting;
mod importing;
mod machine_state;
mod paths;
mod reading;
mod schema;

pub use error::SqliteStoreError;
pub use exporting::SqliteExportSink;
pub(crate) use machine_state::MachineState;
pub use paths::default_database_path;

use schema::initialize;
use std::fs;
use std::path::Path;

use rusqlite::Connection;

pub struct SqliteUsageStore {
    connection: Connection,
}

impl SqliteUsageStore {
    /// Opens an identified usage database or initializes an empty database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SqliteStoreError> {
        let connection = Connection::open(path)?;
        Self::from_connection(connection)
    }

    pub fn open_default() -> Result<Self, SqliteStoreError> {
        let path = default_database_path()?;
        let directory = path
            .parent()
            .ok_or_else(|| SqliteStoreError::InvalidDatabasePath(path.clone()))?;
        fs::create_dir_all(directory).map_err(|source| SqliteStoreError::CreateDataDirectory {
            path: directory.to_owned(),
            source,
        })?;
        Self::open(path)
    }

    pub fn open_in_memory() -> Result<Self, SqliteStoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, SqliteStoreError> {
        initialize(&mut connection)?;
        connection.pragma_update(None, "foreign_keys", true)?;
        Ok(Self { connection })
    }
}
