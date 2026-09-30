mod config;
mod doctor;
mod exporting;
mod reporting;
mod server;
mod uploading;

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
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
    if matches!(command, Command::Help) {
        return io::stdout()
            .lock()
            .write_all(USAGE.as_bytes())
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
    if let Command::ServerReport {
        options,
        period,
        filters,
    } = &command
    {
        let config = if options.url.is_some() && options.auth_file.is_some() {
            config::Config::default()
        } else {
            config::Config::load()?
        };
        let (url, auth_file) = options.resolve(&config)?;
        let summary = server::ServerClient::new(url, auth_file)
            .and_then(|client| client.summary(*period, filters))
            .map_err(CliError::ServerSummary)?;
        return io::stdout()
            .lock()
            .write_all(reporting::render_server_summary(&summary, *period).as_bytes())
            .map_err(CliError::Output);
    }
    let config = config::Config::load()?;
    let uploader = if let Command::Upload(options) = &command {
        let (url, auth_file) = options.resolve(&config)?;
        Some(uploading::Uploader::new(url, auth_file).map_err(CliError::Upload)?)
    } else {
        None
    };
    let mut tracker = TokenTracker::open(TokenTrackerConfig {
        machine_name: config.machine_name,
        ..Default::default()
    })
    .map_err(CliError::Open)?;
    let imported = tracker.refresh().map_err(CliError::Import)?;

    if let Some(uploader) = uploader {
        let snapshot = tracker.export_snapshot().map_err(CliError::Export)?;
        let status = uploader.upload(&snapshot).map_err(CliError::Upload)?;
        return writeln!(
            io::stdout().lock(),
            "Snapshot revision {} {status}.",
            snapshot.export_revision
        )
        .map_err(CliError::Output);
    }

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

    let output = if summary {
        reporting::render_summary(&result.report)
    } else {
        reporting::render_terminal_report(
            &result.report,
            period,
            &imported.warnings,
            &result.diagnostics,
            AGENT_LABELS,
        )
    };
    io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .map_err(CliError::Output)
}

const USAGE: &str =
    "Usage: token-tracker [day|week|month] [filters] [--server [<server-url>] [--auth-file <path>]]
       token-tracker doctor
       token-tracker summary [filters] [--server [<server-url>] [--auth-file <path>]]
       token-tracker export <path> [--force]
       token-tracker upload [<server-url>] [--auth-file <path>]

Without arguments, refresh sources and show the all-time usage report.
Day, week, and month select the current UTC calendar period; weeks start Monday.
Doctor checks configuration, storage, and source imports without changing local data.
Summary refreshes sources and shows token totals by type and total cost.
Use --server to fetch combined totals without accessing local usage.
Filters: --agent <id>, --provider <name>, --model <name> (exact, case-sensitive).
Repeat a filter to match any listed value; different filters combine with AND.
Export and upload refresh sources before exporting or uploading retained usage.
Existing exports require --force. Use -- before paths beginning with '-'.
Server requests require HTTPS (HTTP is allowed for loopback addresses).
Server URL and auth-file default to server_url and auth_file in config.toml.
Command-line values override these defaults.
";

enum Command {
    Report {
        period: ReportingPeriod,
        filters: ReportFilters,
        summary: bool,
    },
    Doctor,
    ServerReport {
        options: ServerOptions,
        period: ReportingPeriod,
        filters: ReportFilters,
    },
    Export {
        path: PathBuf,
        force: bool,
    },
    Upload(ServerOptions),
    Help,
}

struct ServerOptions {
    url: Option<String>,
    auth_file: Option<PathBuf>,
}

impl ServerOptions {
    fn resolve<'a>(&'a self, config: &'a config::Config) -> Result<(&'a str, &'a Path), CliError> {
        let url = self
            .url
            .as_deref()
            .or(config.server_url.as_deref())
            .filter(|url| !url.is_empty())
            .ok_or(CliError::Arguments)?;
        let auth_file = self
            .auth_file
            .as_deref()
            .or(config.auth_file.as_deref())
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or(CliError::Arguments)?;
        Ok((url, auth_file))
    }
}

fn parse_command() -> Result<Command, CliError> {
    let mut args = std::env::args_os().skip(1).peekable();
    let Some(command) = args.next() else {
        return parse_report(ReportingPeriod::AllTime, false, args);
    };
    if (command == "--help" || command == "-h") && args.next().is_none() {
        return Ok(Command::Help);
    }
    if command == "doctor" && args.next().is_none() {
        return Ok(Command::Doctor);
    }
    let period = match command.to_str() {
        Some("day") => Some(ReportingPeriod::Day),
        Some("week") => Some(ReportingPeriod::Week),
        Some("month") => Some(ReportingPeriod::Month),
        Some("summary" | "--server" | "--agent" | "--provider" | "--model") => {
            Some(ReportingPeriod::AllTime)
        }
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
    if command == "upload" {
        let url = if args
            .peek()
            .is_some_and(|arg| !arg.as_encoded_bytes().starts_with(b"-"))
        {
            Some(
                args.next()
                    .and_then(|arg| arg.into_string().ok())
                    .filter(|arg| !arg.is_empty())
                    .ok_or(CliError::Arguments)?,
            )
        } else {
            None
        };
        let auth_file = match args.next() {
            Some(flag) if flag == "--auth-file" => Some(PathBuf::from(
                args.next()
                    .filter(|arg| !arg.is_empty())
                    .ok_or(CliError::Arguments)?,
            )),
            Some(_) => return Err(CliError::Arguments),
            None => None,
        };
        if args.next().is_some() {
            return Err(CliError::Arguments);
        }
        let options = ServerOptions { url, auth_file };
        return Ok(Command::Upload(options));
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

fn parse_report(
    period: ReportingPeriod,
    summary: bool,
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Command, CliError> {
    let mut args = args.into_iter().peekable();
    let mut filters = ReportFilters::default();
    let mut server = false;
    let mut options = ServerOptions {
        url: None,
        auth_file: None,
    };
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--agent" | "--provider" | "--model") => {
                let value = args
                    .next()
                    .and_then(|arg| arg.into_string().ok())
                    .filter(|value| !value.trim().is_empty() && !value.starts_with('-'))
                    .ok_or(CliError::Arguments)?;
                match flag.to_str().unwrap() {
                    "--agent" => filters.agents.push(value),
                    "--provider" => filters.providers.push(value),
                    _ => filters.models.push(value),
                }
            }
            Some("--server") if !server => {
                server = true;
                if args
                    .peek()
                    .is_some_and(|arg| !arg.as_encoded_bytes().starts_with(b"-"))
                {
                    options.url = Some(
                        args.next()
                            .and_then(|arg| arg.into_string().ok())
                            .filter(|value| !value.is_empty())
                            .ok_or(CliError::Arguments)?,
                    );
                }
            }
            Some("--auth-file") if options.auth_file.is_none() => {
                options.auth_file = Some(PathBuf::from(
                    args.next()
                        .filter(|arg| !arg.is_empty() && !arg.as_encoded_bytes().starts_with(b"-"))
                        .ok_or(CliError::Arguments)?,
                ));
            }
            _ => return Err(CliError::Arguments),
        }
    }
    if server {
        Ok(Command::ServerReport {
            options,
            period,
            filters,
        })
    } else if options.auth_file.is_some() {
        Err(CliError::Arguments)
    } else {
        Ok(Command::Report {
            period,
            filters,
            summary,
        })
    }
}

#[derive(Debug)]
enum CliError {
    Arguments,
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
    Upload(uploading::UploadError),
    ServerSummary(server::ServerError),
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
            Self::Upload(source) => write!(formatter, "could not upload snapshot: {source}"),
            Self::ServerSummary(source) => {
                write!(formatter, "could not fetch server summary: {source}")
            }
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
