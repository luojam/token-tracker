use std::error::Error;
use std::io::BufRead;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::domain::{AgentId, ParentSession, SessionMetadata, Timestamp, UsageEvent};

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

/// An adapter-defined diagnostic, retained with the successful import.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParseNotice {
    pub code: String,
    pub message: String,
    pub count: NonZeroU64,
    /// First affected line, one-based.
    pub line: Option<NonZeroU64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSession {
    pub metadata: SessionMetadata,
    pub events: Vec<UsageEvent>,
    pub completion: ParseCompletion,
    pub notices: Vec<ParseNotice>,
}

#[derive(Clone, Copy, Debug)]
pub struct ParseContext<'a> {
    /// Absolute path for source metadata and relative path resolution, never event identity.
    pub source_path: &'a Path,
}

/// The caller handles file I/O and revision checks. Parse only the supplied reader.
pub trait SessionParser {
    type Error: Error + Send + Sync + 'static;

    /// Bump to reimport unchanged files after normalization changes.
    /// Session and event identity changes require a migration.
    fn normalization_version(&self) -> u32 {
        1
    }

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
    pub normalization_version: Option<u32>,
    pub notices: Vec<ParseNotice>,
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionImport {
    pub normalization_version: u32,
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
    DeferredIncomplete,
}

pub trait UsageStore {
    type Error: Error + Send + Sync + 'static;

    fn source_states(&self, agent: &AgentId) -> Result<Vec<SourceState>, Self::Error>;

    /// Updates this agent's observed revisions and presence, preserving successful imports.
    /// Omitted sources are missing only under inspected roots outside inaccessible paths.
    fn record_discovery(
        &mut self,
        agent: &AgentId,
        report: &DiscoveryReport,
        observed_at: Timestamp,
    ) -> Result<(), Self::Error>;

    /// Atomically upserts source/session metadata and observations, retaining omitted history.
    /// Normalization changes are deferred until parsing completes.
    fn commit_import(&mut self, import: &SessionImport)
    -> Result<CommitImportOutcome, Self::Error>;
}

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

    /// Loads unreconciled provenance and observations from one consistent snapshot.
    fn usage_snapshot(&self) -> Result<UsageSnapshot, Self::Error>;
}
