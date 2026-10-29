mod args;
mod config;
mod doctor;
mod exporting;
mod remote;
mod reporting;

use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use self::args::{Cli, Command};
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
    let cli = Cli::parse();
    if matches!(cli.command, Some(Command::Doctor)) {
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
    if matches!(cli.command, Some(Command::Sync)) {
        return remote::sync(&config);
    }

    let report_args = match &cli.command {
        None => Some((&cli.filters, ReportingPeriod::AllTime, false)),
        Some(Command::Day(args)) => Some((args, ReportingPeriod::Day, false)),
        Some(Command::Week(args)) => Some((args, ReportingPeriod::Week, false)),
        Some(Command::Month(args)) => Some((args, ReportingPeriod::Month, false)),
        Some(Command::Summary(args)) => Some((args, ReportingPeriod::AllTime, true)),
        _ => None,
    };
    if let Some((args, period, summary)) = report_args.filter(|(args, _, _)| args.remote) {
        return remote::report(&config, args, period, summary);
    }

    let mut tracker = TokenTracker::open(TokenTrackerConfig {
        machine_name: config.machine_name,
        ..Default::default()
    })
    .map_err(CliError::Open)?;
    let imported = tracker.refresh().map_err(CliError::Import)?;

    let (period, filters, summary) = match cli.command {
        None => (ReportingPeriod::AllTime, cli.filters, false),
        Some(Command::Day(filters)) => (ReportingPeriod::Day, filters, false),
        Some(Command::Week(filters)) => (ReportingPeriod::Week, filters, false),
        Some(Command::Month(filters)) => (ReportingPeriod::Month, filters, false),
        Some(Command::Summary(filters)) => (ReportingPeriod::AllTime, filters, true),
        Some(Command::Export { path, force }) => {
            let snapshot = tracker.export_snapshot().map_err(CliError::Export)?;
            return exporting::write_snapshot(&path, force, &snapshot);
        }
        Some(Command::Sync | Command::Doctor) => {
            unreachable!("sync and doctor are handled above")
        }
    };
    let result = tracker
        .report_filtered(
            period,
            &ReportFilters {
                agents: filters.agents,
                providers: filters.providers,
                models: filters.models,
            },
        )
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

#[derive(Debug)]
enum CliError {
    Doctor,
    Remote(token_tracker::remote::RemoteError),
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
            Self::Remote(source) => write!(formatter, "remote sync/report failed: {source}"),
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
