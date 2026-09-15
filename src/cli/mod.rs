mod config;
mod exporting;
mod reporting;

use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use token_tracker::{
    AGENT_LABELS, ExportError, ImportSynchronizationError, ReportError, SqliteStoreError,
    TokenTracker, TokenTrackerConfig,
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
    let command = parse_command()?;
    if matches!(command, Command::Help) {
        return io::stdout()
            .lock()
            .write_all(USAGE.as_bytes())
            .map_err(CliError::Output);
    }
    let config = config::Config::load()?;
    let mut tracker = TokenTracker::open(TokenTrackerConfig {
        machine_name: config.machine_name,
        ..Default::default()
    })
    .map_err(CliError::Open)?;

    if let Command::Export { path, force } = command {
        let snapshot = tracker.export_snapshot().map_err(CliError::Export)?;
        return exporting::write_snapshot(&path, force, &snapshot);
    }

    let imported = tracker.refresh().map_err(CliError::Import)?;
    let result = tracker.report().map_err(CliError::Report)?;

    let output = reporting::render_terminal_report(
        &result.report,
        &imported.warnings,
        &result.diagnostics,
        AGENT_LABELS,
    );
    io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(CliError::Output)
}

const USAGE: &str = "Usage: token-tracker\n       token-tracker export <path> [--force]\n\nWithout arguments, refresh sources and show the usage report.\nExport writes retained usage to SQLite without refreshing sources.\nExisting exports require --force. Use -- before paths beginning with '-'.\n";

enum Command {
    Report,
    Export { path: PathBuf, force: bool },
    Help,
}

fn parse_command() -> Result<Command, CliError> {
    let mut args = std::env::args_os().skip(1);
    let Some(command) = args.next() else {
        return Ok(Command::Report);
    };
    if (command == "--help" || command == "-h") && args.next().is_none() {
        return Ok(Command::Help);
    }
    if command != "export" {
        return Err(CliError::Arguments);
    }
    let mut path = None;
    let mut force = false;
    let mut positional_only = false;
    for arg in args {
        if !positional_only && arg == "--" {
            positional_only = true;
        } else if !positional_only && arg == "--force" && !force {
            force = true;
        } else if (!positional_only && arg.as_encoded_bytes().starts_with(b"-"))
            || arg.is_empty()
            || path.is_some()
        {
            return Err(CliError::Arguments);
        } else {
            path = Some(PathBuf::from(arg));
        }
    }
    Ok(Command::Export {
        path: path.ok_or(CliError::Arguments)?,
        force,
    })
}

#[derive(Debug)]
enum CliError {
    Arguments,
    Config {
        path: PathBuf,
        source: io::Error,
    },
    Open(SqliteStoreError),
    Import(ImportSynchronizationError),
    Report(ReportError),
    Output(io::Error),
    Export(ExportError),
    ExportOutput {
        path: PathBuf,
        source: io::Error,
    },
    ExportDatabase {
        path: PathBuf,
        source: token_tracker::PublishError<rusqlite::Error>,
    },
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments => write!(formatter, "invalid arguments\n{USAGE}"),
            Self::Config { path, source } => {
                write!(
                    formatter,
                    "could not load config {}: {source}",
                    path.display()
                )
            }
            Self::Open(source) => write!(formatter, "could not open usage storage: {source}"),
            Self::Import(source) => write!(formatter, "session synchronization failed: {source}"),
            Self::Report(source) => write!(formatter, "all-time summary failed: {source}"),
            Self::Output(source) => write!(formatter, "could not write report: {source}"),
            Self::Export(source) => write!(formatter, "could not build export: {source}"),
            Self::ExportDatabase { path, source } => write!(
                formatter,
                "could not write export to {}: {source}",
                path.display()
            ),
            Self::ExportOutput { path, source } => {
                write!(
                    formatter,
                    "could not write export to {}: {source}",
                    path.display()
                )?;
                if source.kind() == io::ErrorKind::AlreadyExists {
                    write!(formatter, "; use --force to overwrite")?;
                }
                Ok(())
            }
        }
    }
}
