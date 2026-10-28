mod config;
mod doctor;
mod exporting;
mod help;
mod reporting;

use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use token_tracker::domain::{ReportFilters, ReportingPeriod};

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
    if let Command::Help(text) = command {
        return io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(CliError::Output);
    }
    if matches!(command, Command::Doctor) {
        let report = doctor::inspect();
        io::stdout()
            .lock()
            .write_all(report.output.as_bytes())
            .map_err(CliError::Output)?;
        return if report.has_issues {
            Err(CliError::Doctor)
        } else {
            Ok(())
        };
    }
    let config = config::Config::load()?;
    let mut tracker = TokenTracker::open(TokenTrackerConfig {
        machine_name: config.machine_name,
        ..Default::default()
    })
    .map_err(CliError::Open)?;
    let imported = tracker.refresh().map_err(CliError::Import)?;

    if let Command::Export { path, force } = command {
        let snapshot = tracker.export_snapshot().map_err(CliError::Export)?;
        return exporting::write_snapshot(&path, force, &snapshot);
    }

    let Command::Report {
        period,
        filters,
        summary,
    } = command
    else {
        unreachable!("other commands return before reporting");
    };
    let result = tracker
        .report_filtered(period, &filters)
        .map_err(CliError::Report)?;

    let width = terminal_width();
    let output = if summary {
        reporting::wrap_report(reporting::render_summary(&result.report), width)
    } else {
        reporting::render_terminal_report(
            &result.report,
            period,
            &imported.warnings,
            &result.diagnostics,
            AGENT_LABELS,
            width,
        )
    };
    io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(CliError::Output)
}

fn terminal_width() -> Option<usize> {
    if !io::stdout().is_terminal() {
        return None;
    }
    terminal_size::terminal_size_of(io::stdout())
        .map(|(terminal_size::Width(width), _)| usize::from(width))
        .filter(|width| *width > 0)
}

enum Command {
    Report {
        period: ReportingPeriod,
        filters: ReportFilters,
        summary: bool,
    },
    Doctor,
    Export {
        path: PathBuf,
        force: bool,
    },
    Help(String),
}

fn parse_command() -> Result<Command, CliError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let wants_help = args
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--help" || arg == "-h");
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return parse_report(ReportingPeriod::AllTime, false, args);
    };
    if wants_help {
        if let Some(text) = command.to_str().and_then(help::for_command) {
            return Ok(Command::Help(text));
        }
    }
    if command == "doctor" {
        if let Some(arg) = args.next() {
            return Err(unexpected(&arg));
        }
        return Ok(Command::Doctor);
    }
    let period = match command.to_str() {
        Some("day") => Some(ReportingPeriod::Day),
        Some("week") => Some(ReportingPeriod::Week),
        Some("month") => Some(ReportingPeriod::Month),
        Some("summary" | "--agent" | "--provider" | "--model") => Some(ReportingPeriod::AllTime),
        _ => None,
    };
    if let Some(period) = period {
        let summary = command == "summary";
        let first_flag = command
            .as_encoded_bytes()
            .starts_with(b"-")
            .then_some(command);
        return parse_report(period, summary, first_flag.into_iter().chain(args));
    }
    if command != "export" {
        return Err(if command.as_encoded_bytes().starts_with(b"-") {
            unexpected(&command)
        } else {
            arguments(format!("unknown command '{}'", command.to_string_lossy()))
        });
    }
    let mut path = None;
    let mut force = false;
    let mut positional_only = false;
    for arg in args {
        if !positional_only && arg == "--" {
            positional_only = true;
        } else if !positional_only && arg == "--force" {
            if force {
                return Err(arguments("--force can only be used once"));
            }
            force = true;
        } else if !positional_only && arg.as_encoded_bytes().starts_with(b"-") {
            return Err(unexpected(&arg));
        } else if arg.is_empty() {
            return Err(arguments("export requires a nonempty path"));
        } else if path.is_some() {
            return Err(arguments("export accepts only one path"));
        } else {
            path = Some(PathBuf::from(arg));
        }
    }
    Ok(Command::Export {
        path: path.ok_or_else(|| arguments("export requires a path"))?,
        force,
    })
}

fn parse_report(
    period: ReportingPeriod,
    summary: bool,
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Command, CliError> {
    let mut args = args.into_iter();
    let mut filters = ReportFilters::default();
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--agent" | "--provider" | "--model") => {
                let value = args
                    .next()
                    .and_then(|arg| arg.into_string().ok())
                    .filter(|value| !value.trim().is_empty() && !value.starts_with('-'))
                    .ok_or_else(|| {
                        arguments(format!("{} requires a value", flag.to_string_lossy()))
                    })?;
                match flag.to_str().unwrap() {
                    "--agent" => filters.agents.push(value),
                    "--provider" => filters.providers.push(value),
                    _ => filters.models.push(value),
                }
            }
            _ => return Err(unexpected(&flag)),
        }
    }
    Ok(Command::Report {
        period,
        filters,
        summary,
    })
}

fn arguments(message: impl Into<String>) -> CliError {
    CliError::Arguments(message.into())
}

fn unexpected(arg: &std::ffi::OsStr) -> CliError {
    let kind = if arg.as_encoded_bytes().starts_with(b"-") {
        "unknown option"
    } else {
        "unexpected argument"
    };
    arguments(format!("{kind} '{}'", arg.to_string_lossy()))
}

#[derive(Debug)]
enum CliError {
    Arguments(String),
    Doctor,
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
            Self::Arguments(message) => {
                write!(
                    formatter,
                    "{message}\nTry 'token-tracker --help' for usage."
                )
            }
            Self::Doctor => formatter.write_str("doctor found issues (see report above)"),
            Self::Config { path, source } => {
                write!(
                    formatter,
                    "could not load config {}: {source}",
                    path.display()
                )
            }
            Self::Open(source) => write!(formatter, "could not open usage storage: {source}"),
            Self::Import(source) => write!(formatter, "session synchronization failed: {source}"),
            Self::Report(source) => write!(formatter, "usage report failed: {source}"),
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
