//! Contracts for session discovery, parsing, persistence, and queries.

use std::error::Error;
use std::io::BufRead;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::core::{SessionMetadata, Timestamp, UsageEvent, UsageSummary};

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

    fn discover(&self) -> Result<DiscoveryReport, Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseCompletion {
    Complete,
    /// The trailing JSONL value was incomplete.
    IncompleteFinalLine,
}

/// Parsed metadata and usage, excluding source contents.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSession {
    pub metadata: SessionMetadata,
    pub events: Vec<UsageEvent>,
    pub completion: ParseCompletion,
}

/// The caller handles file I/O and revision checks.
pub trait SessionParser {
    type Error: Error + Send + Sync + 'static;

    fn parse(&self, input: &mut dyn BufRead) -> Result<ParsedSession, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceState {
    pub path: PathBuf,
    /// Latest discovered revision, even if import failed.
    pub last_observed_revision: FileRevision,
    /// Latest committed revision.
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

pub trait UsageStore {
    type Error: Error + Send + Sync + 'static;

    fn source_states(&self) -> Result<Vec<SourceState>, Self::Error>;

    /// Updates observed revisions and presence without changing successful imports.
    /// An omitted source is missing only when under an inspected root and not at
    /// or below an inaccessible path.
    fn record_discovery(
        &mut self,
        report: &DiscoveryReport,
        observed_at: Timestamp,
    ) -> Result<(), Self::Error>;

    /// Atomically upserts the latest metadata for a source path and its
    /// source-scoped observations. A newer import may replace metadata from a
    /// different session; observations absent from a rewritten source are retained.
    fn commit_import(&mut self, import: &SessionImport) -> Result<ImportStats, Self::Error>;
}

pub trait UsageSummaryStore {
    type Error: Error + Send + Sync + 'static;

    /// Reconciles observations by logical event identity using stable provenance,
    /// independent of processing order and transient file state.
    fn all_time_summary(&self) -> Result<UsageSummary, Self::Error>;
}
