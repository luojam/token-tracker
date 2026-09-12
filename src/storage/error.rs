use std::{error::Error, fmt, io, path::PathBuf};

#[derive(Debug)]
pub enum SqliteStoreError {
    Sqlite(rusqlite::Error),
    CreateDataDirectory { path: PathBuf, source: io::Error },
    HomeDirectoryUnavailable,
    InvalidDatabasePath(PathBuf),
    UnsupportedSchemaVersion(i64),
    ValueOutOfRange(&'static str),
    CorruptData(&'static str),
    Serialization(serde_json::Error),
}

impl fmt::Display for SqliteStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(source) => write!(formatter, "SQLite storage error: {source}"),
            Self::CreateDataDirectory { path, source } => {
                write!(formatter, "could not create {}: {source}", path.display())
            }
            Self::HomeDirectoryUnavailable => formatter.write_str(
                "HOME is unavailable or invalid and XDG_DATA_HOME is not an absolute path",
            ),
            Self::InvalidDatabasePath(path) => {
                write!(formatter, "database path has no parent: {}", path.display())
            }
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "database schema version {version} is incompatible; recreate the database"
                )
            }
            Self::ValueOutOfRange(value) => write!(formatter, "{value} is out of range"),
            Self::CorruptData(message) => write!(formatter, "database contains {message}"),
            Self::Serialization(source) => {
                write!(formatter, "could not encode stored data: {source}")
            }
        }
    }
}

impl Error for SqliteStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(source) => Some(source),
            Self::Serialization(source) => Some(source),
            Self::CreateDataDirectory { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for SqliteStoreError {
    fn from(source: rusqlite::Error) -> Self {
        Self::Sqlite(source)
    }
}
