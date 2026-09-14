mod parsing;
mod reading;
mod source;

use std::{error::Error, fmt};

use crate::application::{SessionSnapshot, SourceKey};

pub use reading::read_snapshot;
pub use source::HermesSessionSource;

const HERMES_AGENT_ID: &str = "hermes";

/// One complete database read. Rejected sessions must retain their last good import.
#[derive(Debug)]
pub struct HermesDatabaseSnapshot {
    pub sessions: Vec<HermesSessionSnapshot>,
}

#[derive(Debug)]
pub struct HermesSessionSnapshot {
    pub key: SourceKey,
    pub snapshot: Result<SessionSnapshot, HermesReadError>,
}

#[derive(Debug)]
pub enum HermesReadError {
    Sqlite(rusqlite::Error),
    UnsupportedSchema {
        table: &'static str,
        missing: Vec<&'static str>,
    },
    InvalidField(&'static str),
    InconsistentAccounting(&'static str),
    UncachedSnapshot,
}

impl fmt::Display for HermesReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "could not read Hermes accounting snapshot: {error}"),
            Self::UnsupportedSchema { table, missing } => write!(
                f,
                "unsupported Hermes schema: {table} is missing {}; use a modern database with per-model/task accounting",
                missing.join(", ")
            ),
            Self::InvalidField(field) => write!(f, "invalid Hermes accounting field: {field}"),
            Self::InconsistentAccounting(message) => {
                write!(f, "invalid Hermes accounting: {message}")
            }
            Self::UncachedSnapshot => {
                f.write_str("Hermes snapshot is no longer cached; discover sessions again")
            }
        }
    }
}

impl Error for HermesReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for HermesReadError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
