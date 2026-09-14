use std::error::Error;
use std::fmt;

use crate::application::AllTimeReportError;
use crate::storage::SqliteStoreError;
use crate::{TokenTracker, TokenTrackerConfig};

pub fn run() -> Result<String, TokenTrackerError> {
    let mut tracker = TokenTracker::open(TokenTrackerConfig::default())
        .map_err(TokenTrackerError::StorageSetup)?;
    let imported = tracker.refresh().map_err(|source| {
        TokenTrackerError::Workflow(AllTimeReportError::Synchronization(source))
    })?;
    let result = tracker.report().map_err(|source| {
        TokenTrackerError::Workflow(AllTimeReportError::Summary(Box::new(source)))
    })?;
    Ok(crate::cli::render_terminal_report(
        &result.report,
        &imported.warnings,
        &[
            ("hermes", "Hermes"),
            ("pi", "Pi"),
            ("codex", "Codex"),
            ("claude", "Claude Code"),
        ],
    ))
}

#[derive(Debug)]
pub enum TokenTrackerError {
    StorageSetup(SqliteStoreError),
    Workflow(AllTimeReportError),
}

impl fmt::Display for TokenTrackerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StorageSetup(source) => {
                write!(formatter, "could not open usage storage: {source}")
            }
            Self::Workflow(source) => source.fmt(formatter),
        }
    }
}

impl Error for TokenTrackerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::StorageSetup(source) => Some(source),
            Self::Workflow(source) => Some(source),
        }
    }
}
