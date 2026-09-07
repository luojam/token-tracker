pub mod pricing;
mod reconciliation;
mod reporting;
mod synchronization;
mod workflow;

pub use reconciliation::{SummaryError, summarize_usage};
pub use reporting::render_terminal_report;
pub use synchronization::{
    ImportCounts, ImportSynchronizationError, ImportWarning, SynchronizationReport,
    synchronize_sessions, synchronize_sessions_at,
};
pub use workflow::{AllTimeReportError, ImportAdapter, SessionAdapter, run_all_time_report};

use std::error::Error;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::core::{AgentId, ParentSession, SessionMetadata, Timestamp, UsageEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRevision {
    pub size: u64,
    pub modified_at: SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredSessionFile {
    pub path: PathBuf,
    pub revision: FileRevision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryWarning {
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryCoverage {
    pub inspected_roots: Vec<PathBuf>,
    pub inaccessible_paths: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub files: Vec<DiscoveredSessionFile>,
    pub warnings: Vec<DiscoveryWarning>,
    pub coverage: DiscoveryCoverage,
}

pub trait SessionDiscovery {
    type Error: Error + Send + Sync + 'static;

    /// Stable namespace for this adapter, including files not yet parsed.
    fn agent_id(&self) -> AgentId;

    fn discover(&self) -> Result<DiscoveryReport, Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseCompletion {
    Complete,
    IncompleteFinalLine,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSession {
    pub metadata: SessionMetadata,
    pub events: Vec<UsageEvent>,
    pub completion: ParseCompletion,
}

#[derive(Clone, Copy, Debug)]
pub struct ParseContext<'a> {
    /// Absolute source path, for filename metadata and relative path resolution.
    /// Do not include its location in logical usage event identities.
    pub source_path: &'a Path,
}

/// The caller handles file I/O and revision checks. Parse only the supplied reader.
pub trait SessionParser {
    type Error: Error + Send + Sync + 'static;

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceState {
    pub path: PathBuf,
    /// Latest discovered revision, even if import failed.
    pub last_observed_revision: FileRevision,
    pub last_imported_revision: Option<FileRevision>,
    pub last_successful_scan: Option<Timestamp>,
    pub last_parse_completion: Option<ParseCompletion>,
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionImport {
    pub source: DiscoveredSessionFile,
    pub scanned_at: Timestamp,
    pub parsed: ParsedSession,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportStats {
    pub event_identities_inserted: u64,
    pub observations_inserted: u64,
    pub observations_updated: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitImportOutcome {
    Applied(ImportStats),
    IgnoredStale,
}

pub trait UsageStore {
    type Error: Error + Send + Sync + 'static;

    /// Only source state owned by this agent.
    fn source_states(&self, agent: &AgentId) -> Result<Vec<SourceState>, Self::Error>;

    /// Updates observed revisions and presence without changing successful imports.
    /// Only sources owned by `agent` are affected.
    /// An omitted source is missing only when under an inspected root and not at
    /// or below an inaccessible path.
    fn record_discovery(
        &mut self,
        agent: &AgentId,
        report: &DiscoveryReport,
        observed_at: Timestamp,
    ) -> Result<(), Self::Error>;

    /// Atomically upserts the latest metadata for a source path and the
    /// observations for that source/session pair. Historical session provenance
    /// and observations absent from a rewritten source are retained.
    fn commit_import(&mut self, import: &SessionImport)
    -> Result<CommitImportOutcome, Self::Error>;
}

/// A source's historical session identity, independent of database row IDs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceSessionKey {
    pub agent: AgentId,
    pub session_id: String,
    pub source_path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionProvenance {
    pub key: SourceSessionKey,
    pub started_at: Timestamp,
    pub parent_session: Option<ParentSession>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageObservation {
    pub session: SourceSessionKey,
    pub event: UsageEvent,
}

/// One provenance record per source/session and one observation per
/// (source/session, event identity). Record order carries no meaning.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSnapshot {
    pub sessions: Vec<SessionProvenance>,
    pub observations: Vec<UsageObservation>,
}

pub trait UsageReadStore {
    type Error: Error + Send + Sync + 'static;

    /// Loads provenance and observations from one consistent storage snapshot.
    /// Reconciliation and aggregation belong to the application.
    fn usage_snapshot(&self) -> Result<UsageSnapshot, Self::Error>;
}
