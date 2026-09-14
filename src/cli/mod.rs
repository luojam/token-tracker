mod reporting;

use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;

use token_tracker::{
    AGENT_LABELS, ImportSynchronizationError, ReportError, SqliteStoreError, TokenTracker,
    TokenTrackerConfig,
};

pub fn run() -> ExitCode {
    match execute() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("token-tracker: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute() -> Result<(), CliError> {
    let mut tracker = TokenTracker::open(TokenTrackerConfig::default()).map_err(CliError::Open)?;
    let imported = tracker.refresh().map_err(CliError::Import)?;
    let result = tracker.report().map_err(CliError::Report)?;

    let output =
        reporting::render_terminal_report(&result.report, &imported.warnings, AGENT_LABELS);
    io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(CliError::Output)
}

#[derive(Debug)]
enum CliError {
    Open(SqliteStoreError),
    Import(ImportSynchronizationError),
    Report(ReportError),
    Output(io::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(source) => write!(formatter, "could not open usage storage: {source}"),
            Self::Import(source) => write!(formatter, "session synchronization failed: {source}"),
            Self::Report(source) => write!(formatter, "all-time summary failed: {source}"),
            Self::Output(source) => write!(formatter, "could not write report: {source}"),
        }
    }
}
