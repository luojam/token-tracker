use std::error::Error;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;

use super::ValidatedSessionImport;
use crate::domain::{AgentId, ParentSession, SessionMetadata, Timestamp, UsageEvent};

/// Stable source identity defined by the adapter, scoped by agent.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceKey(pub Vec<u8>);

/// Compare only for equality: equal tokens mean unchanged metadata and usage across scans and restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRevision(pub Vec<u8>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredSource {
    pub key: SourceKey,
    pub revision: SourceRevision,
    /// For diagnostics and parent references; never identity.
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryWarning {
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub sources: Vec<DiscoveredSource>,
    /// Confirmed absent sources only; failed or partial discovery cannot establish absence.
    pub missing_sources: Vec<SourceKey>,
    pub warnings: Vec<DiscoveryWarning>,
}

pub trait SessionSource {
    type Error: Error + Send + Sync + 'static;

    fn agent_id(&self) -> AgentId;

    /// Bump to reimport unchanged sources after normalization changes.
    fn normalization_version(&self) -> NonZeroU32 {
        NonZeroU32::MIN
    }

    /// Known states belong to this agent; returned source keys must be unique.
    fn discover(&self, known: &[SourceState]) -> Result<DiscoveryReport, Self::Error>;

    /// Return data and revision from one consistent read, possibly newer than discovery.
    fn load(&self, source: &DiscoveredSource) -> Result<SessionSnapshot, Self::Error>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionSnapshot {
    pub revision: SourceRevision,
    pub session: SessionData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotCompletion {
    /// All available data was read; the session may still be active.
    Complete,
    /// Usable subset; retry even if the revision is unchanged.
    Partial,
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
pub struct SessionData {
    pub metadata: SessionMetadata,
    pub events: Vec<UsageEvent>,
    pub completion: SnapshotCompletion,
    pub notices: Vec<ParseNotice>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceState {
    pub key: SourceKey,
    pub path: Option<PathBuf>,
    /// Latest discovered revision, even if import failed.
    pub last_observed_revision: SourceRevision,
    pub last_import: Option<SuccessfulImport>,
    pub present: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuccessfulImport {
    pub revision: SourceRevision,
    pub scanned_at: Timestamp,
    pub completion: SnapshotCompletion,
    pub normalization_version: NonZeroU32,
    pub notices: Vec<ParseNotice>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionImport {
    pub normalization_version: NonZeroU32,
    pub source: DiscoveredSource,
    pub scanned_at: Timestamp,
    pub session: SessionData,
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

    /// Updates observed revisions and presence for this agent, preserving successful imports.
    /// Marks only explicit missing sources absent; discovered sources take precedence.
    fn record_discovery(
        &mut self,
        agent: &AgentId,
        report: &DiscoveryReport,
        observed_at: Timestamp,
    ) -> Result<(), Self::Error>;

    /// Atomically upserts source/session metadata and observations, retaining omitted history.
    /// Defers normalization changes until the snapshot is complete.
    fn commit_import(
        &mut self,
        import: &ValidatedSessionImport,
    ) -> Result<CommitImportOutcome, Self::Error>;
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceSessionKey {
    pub agent: AgentId,
    pub session_id: String,
    pub source: SourceKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionProvenance {
    pub key: SourceSessionKey,
    pub source_path: Option<PathBuf>,
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
