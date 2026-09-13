use std::error::Error;
use std::fmt;

use adapters::claude::{CLAUDE_AGENT_ID, ClaudeSessionDiscovery, ClaudeSessionParser};
use adapters::codex::{CODEX_AGENT_ID, CodexSessionDiscovery, CodexSessionParser};
use adapters::files::FileSessionSource;
use adapters::pi::{PI_AGENT_ID, PiSessionDiscovery, PiSessionParser};
use application::{
    AllTimeReportError, ImportAdapter, ImportWarning, build_usage_report, run_all_time_report,
};
use storage::{SqliteStoreError, SqliteUsageStore};

pub mod adapters;
pub mod application;
pub mod cli;
pub mod domain;
pub mod pricing;
pub mod storage;

type AdapterFactory =
    fn() -> Result<Box<dyn ImportAdapter<SqliteUsageStore>>, Box<dyn Error + Send + Sync>>;

struct AdapterRegistration {
    id: &'static str,
    label: &'static str,
    factory: AdapterFactory,
}

const ADAPTERS: &[AdapterRegistration] = &[
    AdapterRegistration {
        id: PI_AGENT_ID,
        label: "Pi",
        factory: || {
            Ok(Box::new(FileSessionSource::new(
                PiSessionDiscovery::for_default_root()?,
                PiSessionParser::new(),
            )))
        },
    },
    AdapterRegistration {
        id: CODEX_AGENT_ID,
        label: "Codex",
        factory: || {
            Ok(Box::new(FileSessionSource::new(
                CodexSessionDiscovery::for_default_roots()?,
                CodexSessionParser::new(),
            )))
        },
    },
    AdapterRegistration {
        id: CLAUDE_AGENT_ID,
        label: "Claude Code",
        factory: || {
            Ok(Box::new(FileSessionSource::new(
                ClaudeSessionDiscovery::for_default_root()?,
                ClaudeSessionParser::new(),
            )))
        },
    },
];

pub fn run() -> Result<String, TokenTrackerError> {
    let mut store = SqliteUsageStore::open_default().map_err(TokenTrackerError::StorageSetup)?;
    let mut warnings = Vec::new();
    let mut adapters = Vec::new();
    for registration in ADAPTERS {
        match (registration.factory)() {
            Ok(adapter) => adapters.push(adapter),
            Err(source) => warnings.push(ImportWarning {
                path: None,
                message: format!("{}: could not configure adapter: {source}", registration.id),
            }),
        }
    }
    let adapters = adapters.iter().map(Box::as_ref).collect::<Vec<_>>();
    let report = run_all_time_report(&adapters, &mut store, warnings)
        .map_err(TokenTrackerError::Workflow)?;
    let agent_labels = ADAPTERS
        .iter()
        .map(|registration| (registration.id, registration.label))
        .collect::<Vec<_>>();
    Ok(cli::render_terminal_report(
        &build_usage_report(&report.summary),
        &report.warnings,
        &agent_labels,
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
