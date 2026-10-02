mod config;
mod doctor;
mod exporting;
mod help;
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
    Help(String),
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
            .ok_or_else(|| {
                arguments("missing server URL; supply a URL or set server_url in config")
            })?;
        let auth_file = self
            .auth_file
            .as_deref()
            .or(config.auth_file.as_deref())
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| {
                arguments(
                    "missing bearer-token file; use --auth-file <path> or set auth_file in config",
                )
            })?;
        Ok((url, auth_file))
    }
}

fn parse_command() -> Result<Command, CliError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let wants_help = args
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--help" || arg == "-h");
    let mut args = args.into_iter().peekable();
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
                    .ok_or_else(|| arguments("server URL must be nonempty UTF-8 text"))?,
            )
        } else {
            None
        };
        let auth_file = match args.next() {
            Some(flag) if flag == "--auth-file" => Some(PathBuf::from(
                args.next()
                    .filter(|arg| !arg.is_empty() && !arg.as_encoded_bytes().starts_with(b"-"))
                    .ok_or_else(|| arguments("--auth-file requires a path"))?,
            )),
            Some(arg) => return Err(unexpected(&arg)),
            None => None,
        };
        if let Some(arg) = args.next() {
            return Err(unexpected(&arg));
        }
        let options = ServerOptions { url, auth_file };
        return Ok(Command::Upload(options));
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
                    .ok_or_else(|| {
                        arguments(format!("{} requires a value", flag.to_string_lossy()))
                    })?;
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
                            .ok_or_else(|| arguments("server URL must be nonempty UTF-8 text"))?,
                    );
                }
            }
            Some("--auth-file") if options.auth_file.is_none() => {
                options.auth_file = Some(PathBuf::from(
                    args.next()
                        .filter(|arg| !arg.is_empty() && !arg.as_encoded_bytes().starts_with(b"-"))
                        .ok_or_else(|| arguments("--auth-file requires a path"))?,
                ));
            }
            Some("--server" | "--auth-file") => {
                return Err(arguments(format!(
                    "{} can only be used once",
                    flag.to_string_lossy()
                )));
            }
            _ => return Err(unexpected(&flag)),
        }
    }
    if server {
        Ok(Command::ServerReport {
            options,
            period,
            filters,
        })
    } else if options.auth_file.is_some() {
        Err(arguments("--auth-file requires --server for reports"))
    } else {
        Ok(Command::Report {
            period,
            filters,
            summary,
        })
    }
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
