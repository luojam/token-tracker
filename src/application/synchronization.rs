//! Session discovery and import synchronization use case.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    CommitImportOutcome, DiscoveredSessionFile, ImportStats, ParseCompletion, ParsedSession,
    SessionDiscovery, SessionImport, SessionParser, SourceState, UsageStore,
};
use crate::core::Timestamp;

const STABLE_READ_ATTEMPTS: usize = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportCounts {
    pub files_discovered: u64,
    pub files_imported: u64,
    pub files_unchanged: u64,
    pub files_failed: u64,
    pub incomplete_files_imported: u64,
    pub event_identities_inserted: u64,
    pub observations_inserted: u64,
    pub observations_updated: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportWarning {
    pub path: Option<PathBuf>,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SynchronizationReport {
    pub counts: ImportCounts,
    pub warnings: Vec<ImportWarning>,
}

/// A failure that prevents the synchronization operation as a whole.
#[derive(Debug)]
pub enum ImportSynchronizationError {
    Discovery(Box<dyn Error + Send + Sync>),
    Storage {
        operation: &'static str,
        source: Box<dyn Error + Send + Sync>,
    },
}

impl fmt::Display for ImportSynchronizationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(source) => write!(formatter, "session discovery failed: {source}"),
            Self::Storage { operation, source } => {
                write!(formatter, "storage failed while {operation}: {source}")
            }
        }
    }
}

impl Error for ImportSynchronizationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Discovery(source) => Some(source.as_ref()),
            Self::Storage { source, .. } => Some(source.as_ref()),
        }
    }
}

/// Discovers sessions and synchronizes all changed sources using the current time.
pub fn synchronize_sessions<D, P, S>(
    discovery: &D,
    parser: &P,
    store: &mut S,
) -> Result<SynchronizationReport, ImportSynchronizationError>
where
    D: SessionDiscovery,
    P: SessionParser,
    S: UsageStore,
{
    synchronize_sessions_at(discovery, parser, store, current_timestamp())
}

/// Synchronizes sessions at an injected scan time, primarily for deterministic callers and tests.
pub fn synchronize_sessions_at<D, P, S>(
    discovery: &D,
    parser: &P,
    store: &mut S,
    scanned_at: Timestamp,
) -> Result<SynchronizationReport, ImportSynchronizationError>
where
    D: SessionDiscovery,
    P: SessionParser,
    S: UsageStore,
{
    let discovery_report = discovery
        .discover()
        .map_err(|source| ImportSynchronizationError::Discovery(Box::new(source)))?;
    let states = store
        .source_states()
        .map_err(|source| ImportSynchronizationError::Storage {
            operation: "loading source state",
            source: Box::new(source),
        })?;

    // Record presence and observed revisions even when individual files cannot be
    // read or parsed. Successful import state remains separate in the store.
    store
        .record_discovery(&discovery_report, scanned_at)
        .map_err(|source| ImportSynchronizationError::Storage {
            operation: "recording discovery",
            source: Box::new(source),
        })?;

    let known_sources = states
        .into_iter()
        .map(|state| (state.path.clone(), state))
        .collect::<HashMap<_, _>>();
    let mut files = discovery_report.files.clone();
    files.sort_by(|left, right| left.path.cmp(&right.path));

    let mut report = SynchronizationReport {
        counts: ImportCounts {
            files_discovered: files.len() as u64,
            ..ImportCounts::default()
        },
        warnings: discovery_report
            .warnings
            .into_iter()
            .map(|warning| ImportWarning {
                path: warning.path,
                message: warning.message,
            })
            .collect(),
    };

    for file in files {
        if known_sources
            .get(&file.path)
            .is_some_and(|state| source_is_unchanged(state, &file))
        {
            report.counts.files_unchanged += 1;
            continue;
        }

        let (parsed, revision) = match load_stable_session(&file.path, parser) {
            Ok(result) => result,
            Err(error) => {
                report.counts.files_failed += 1;
                report.warnings.push(ImportWarning {
                    path: Some(file.path),
                    message: error.to_string(),
                });
                continue;
            }
        };

        let incomplete = parsed.completion == ParseCompletion::IncompleteFinalLine;
        let import = SessionImport {
            source: DiscoveredSessionFile {
                path: file.path.clone(),
                revision,
            },
            scanned_at,
            parsed,
        };

        match store.commit_import(&import) {
            Ok(CommitImportOutcome::Applied(stats)) => {
                report.counts.files_imported += 1;
                if incomplete {
                    report.counts.incomplete_files_imported += 1;
                }
                add_import_stats(&mut report.counts, stats);
            }
            Ok(CommitImportOutcome::IgnoredStale) => {
                report.counts.files_failed += 1;
                report.warnings.push(ImportWarning {
                    path: Some(file.path),
                    message: "session import was superseded by a newer scan".into(),
                });
            }
            Err(error) => {
                report.counts.files_failed += 1;
                report.warnings.push(ImportWarning {
                    path: Some(file.path),
                    message: format!("could not store session import: {error}"),
                });
            }
        }
    }

    report.warnings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.message.cmp(&right.message))
    });
    Ok(report)
}

fn source_is_unchanged(state: &SourceState, discovered: &DiscoveredSessionFile) -> bool {
    !state.reimport_required
        && state.last_imported_revision.as_ref() == Some(&discovered.revision)
        && state.last_parse_completion == Some(ParseCompletion::Complete)
}

fn add_import_stats(counts: &mut ImportCounts, stats: ImportStats) {
    counts.event_identities_inserted = counts
        .event_identities_inserted
        .saturating_add(stats.event_identities_inserted);
    counts.observations_inserted = counts
        .observations_inserted
        .saturating_add(stats.observations_inserted);
    counts.observations_updated = counts
        .observations_updated
        .saturating_add(stats.observations_updated);
}

fn load_stable_session<P: SessionParser>(
    path: &Path,
    parser: &P,
) -> Result<(ParsedSession, super::FileRevision), SourceLoadError> {
    let mut last_retry = SourceLoadError::ChangedDuringRead;

    for _ in 0..STABLE_READ_ATTEMPTS {
        match load_session_once(path, parser) {
            LoadAttempt::Stable(result) => return result,
            LoadAttempt::Retry(error) => last_retry = error,
        }
    }

    Err(last_retry)
}

fn load_session_once<P: SessionParser>(path: &Path, parser: &P) -> LoadAttempt {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(source) => return LoadAttempt::Retry(SourceLoadError::Io(source)),
    };
    let handle_before = match file.metadata() {
        Ok(metadata) => metadata,
        Err(source) => return LoadAttempt::Retry(SourceLoadError::Io(source)),
    };
    let path_before = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) => return LoadAttempt::Retry(SourceLoadError::Io(source)),
    };
    if !same_file_and_revision(&handle_before, &path_before) || !path_before.is_file() {
        return LoadAttempt::Retry(SourceLoadError::ChangedDuringRead);
    }

    let revision = match revision_from_metadata(&handle_before) {
        Ok(revision) => revision,
        Err(source) => return LoadAttempt::Retry(SourceLoadError::Io(source)),
    };
    let mut reader = BufReader::new(file);
    let parsed = parser
        .parse(&mut reader)
        .map_err(|error| SourceLoadError::Parse(error.to_string()));

    let handle_after = match reader.get_ref().metadata() {
        Ok(metadata) => metadata,
        Err(source) => return LoadAttempt::Retry(SourceLoadError::Io(source)),
    };
    let path_after = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) => return LoadAttempt::Retry(SourceLoadError::Io(source)),
    };
    if !same_file_and_revision(&handle_before, &handle_after)
        || !same_file_and_revision(&handle_before, &path_after)
    {
        return LoadAttempt::Retry(SourceLoadError::ChangedDuringRead);
    }

    LoadAttempt::Stable(parsed.map(|parsed| (parsed, revision)))
}

fn revision_from_metadata(metadata: &Metadata) -> io::Result<super::FileRevision> {
    Ok(super::FileRevision {
        size: metadata.len(),
        modified_at: metadata.modified()?,
    })
}

fn same_file_and_revision(left: &Metadata, right: &Metadata) -> bool {
    left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
        && same_file_identity(left, right)
}

#[cfg(unix)]
fn same_file_identity(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(_left: &Metadata, _right: &Metadata) -> bool {
    true
}

enum LoadAttempt {
    Stable(Result<(ParsedSession, super::FileRevision), SourceLoadError>),
    Retry(SourceLoadError),
}

#[derive(Debug)]
enum SourceLoadError {
    Io(io::Error),
    ChangedDuringRead,
    Parse(String),
}

impl fmt::Display for SourceLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "could not read session file: {source}"),
            Self::ChangedDuringRead => {
                formatter.write_str("session file kept changing while being read; import deferred")
            }
            Self::Parse(source) => write!(formatter, "could not parse session file: {source}"),
        }
    }
}

fn current_timestamp() -> Timestamp {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    Timestamp::from_unix_milliseconds(i64::try_from(milliseconds).unwrap_or(i64::MAX))
}
