use std::error::Error;
use std::path::PathBuf;

use super::workflow::{import_adapters, read_summary};
use super::{
    ImportAdapter, ImportSynchronizationError, ImportWarning, ReportDiagnostic, ReportError,
    SynchronizationReport, UsageReport, build_usage_report,
};
use crate::adapters::claude::{CLAUDE_AGENT_ID, ClaudeSessionDiscovery, ClaudeSessionParser};
use crate::adapters::codex::{CODEX_AGENT_ID, CodexSessionDiscovery, CodexSessionParser};
use crate::adapters::files::FileSessionSource;
use crate::adapters::hermes::{HERMES_AGENT_ID, HermesSessionSource};
use crate::adapters::pi::{PI_AGENT_ID, PiSessionDiscovery, PiSessionParser};
use crate::storage::{SqliteStoreError, SqliteUsageStore};

/// Agent identifiers and display names for the bundled sources.
pub const AGENT_LABELS: &[(&str, &str)] = &[
    (HERMES_AGENT_ID, "Hermes"),
    (PI_AGENT_ID, "Pi"),
    (CODEX_AGENT_ID, "Codex"),
    (CLAUDE_AGENT_ID, "Claude Code"),
];

#[derive(Clone, Debug)]
pub struct TokenTrackerConfig {
    /// `None` uses the default database path and creates its parent directories.
    /// Parent directories are not created for explicit paths.
    pub database_path: Option<PathBuf>,
    /// `refresh()` does nothing when this list is empty.
    pub sources: Vec<LocalSourceConfig>,
}

impl Default for TokenTrackerConfig {
    fn default() -> Self {
        Self {
            database_path: None,
            sources: vec![
                LocalSourceConfig::Hermes { databases: None },
                LocalSourceConfig::Pi { root: None },
                LocalSourceConfig::Codex { roots: None },
                LocalSourceConfig::Claude { root: None },
            ],
        }
    }
}

/// `None` resolves defaults from the environment on each refresh.
/// Explicit paths are session directories, or SQLite files for Hermes.
#[derive(Clone, Debug)]
pub enum LocalSourceConfig {
    Hermes { databases: Option<Vec<PathBuf>> },
    Pi { root: Option<PathBuf> },
    Codex { roots: Option<Vec<PathBuf>> },
    Claude { root: Option<PathBuf> },
}

impl LocalSourceConfig {
    fn agent_id(&self) -> &'static str {
        match self {
            Self::Hermes { .. } => HERMES_AGENT_ID,
            Self::Pi { .. } => PI_AGENT_ID,
            Self::Codex { .. } => CODEX_AGENT_ID,
            Self::Claude { .. } => CLAUDE_AGENT_ID,
        }
    }

    fn configure(
        &self,
    ) -> Result<Box<dyn ImportAdapter<SqliteUsageStore>>, Box<dyn Error + Send + Sync>> {
        Ok(match self {
            Self::Hermes { databases } => Box::new(match databases {
                Some(databases) => HermesSessionSource::new(databases.clone()),
                None => HermesSessionSource::for_default_roots()?,
            }),
            Self::Pi { root } => Box::new(FileSessionSource::new(
                match root {
                    Some(root) => PiSessionDiscovery::new(root),
                    None => PiSessionDiscovery::for_default_root()?,
                },
                PiSessionParser::new(),
            )),
            Self::Codex { roots } => Box::new(FileSessionSource::new(
                match roots {
                    Some(roots) => CodexSessionDiscovery::new(roots),
                    None => CodexSessionDiscovery::for_default_roots()?,
                },
                CodexSessionParser::new(),
            )),
            Self::Claude { root } => Box::new(FileSessionSource::new(
                match root {
                    Some(root) => ClaudeSessionDiscovery::new(root),
                    None => ClaudeSessionDiscovery::for_default_root()?,
                },
                ClaudeSessionParser::new(),
            )),
        })
    }
}

pub struct TokenTracker {
    store: SqliteUsageStore,
    sources: Vec<LocalSourceConfig>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportResult {
    pub report: UsageReport,
    /// Stored parse notices, including those from missing sources.
    pub diagnostics: Vec<ReportDiagnostic>,
}

impl TokenTracker {
    /// Opens or creates storage without accessing local sources.
    pub fn open(config: TokenTrackerConfig) -> Result<Self, SqliteStoreError> {
        let store = match config.database_path {
            Some(path) => SqliteUsageStore::open(path)?,
            None => SqliteUsageStore::open_default()?,
        };
        Ok(Self {
            store,
            sources: config.sources,
        })
    }

    /// Source failures and retained parse notices become warnings.
    /// Storage failures abort the refresh.
    pub fn refresh(&mut self) -> Result<SynchronizationReport, ImportSynchronizationError> {
        let mut warnings = Vec::new();
        let mut adapters = Vec::new();
        for config in &self.sources {
            match config.configure() {
                Ok(adapter) => adapters.push(adapter),
                Err(source) => warnings.push(ImportWarning {
                    path: None,
                    message: format!(
                        "{}: could not configure adapter: {source}",
                        config.agent_id()
                    ),
                }),
            }
        }

        let adapters = adapters.iter().map(Box::as_ref).collect::<Vec<_>>();
        import_adapters(&adapters, &mut self.store, warnings)
    }

    /// Reads stored usage and diagnostics without refreshing or changing stored state.
    pub fn report(&self) -> Result<ReportResult, ReportError> {
        let (summary, diagnostics) = read_summary(&self.store)?;
        Ok(ReportResult {
            report: build_usage_report(&summary),
            diagnostics,
        })
    }
}
