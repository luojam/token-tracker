use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, BufRead, BufReader};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::application::{
    DiscoveredSource, DiscoveryReport, DiscoveryWarning, SessionData, SessionSnapshot,
    SessionSource, SourceKey, SourceRevision, SourceState,
};
use crate::domain::AgentId;

const STABLE_READ_ATTEMPTS: usize = 2;

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
pub struct FileDiscoveryCoverage {
    pub inspected_roots: Vec<PathBuf>,
    pub inaccessible_paths: Vec<PathBuf>,
}

impl FileDiscoveryCoverage {
    fn covers(&self, path: &Path) -> bool {
        self.inspected_roots
            .iter()
            .any(|root| path.starts_with(root))
            && !self
                .inaccessible_paths
                .iter()
                .any(|blocked| path.starts_with(blocked))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiscoveryReport {
    pub files: Vec<DiscoveredSessionFile>,
    pub warnings: Vec<DiscoveryWarning>,
    pub coverage: FileDiscoveryCoverage,
}

pub trait SessionFileDiscovery {
    type Error: Error + Send + Sync + 'static;

    fn agent_id(&self) -> AgentId;

    fn discover(&self) -> Result<FileDiscoveryReport, Self::Error>;
}

#[derive(Clone, Copy, Debug)]
pub struct ParseContext<'a> {
    /// Absolute path for metadata and relative paths; never event identity.
    pub source_path: &'a Path,
}

/// Parses only the supplied reader; the caller handles I/O and revision checks.
pub trait SessionParser {
    type Error: Error + Send + Sync + 'static;

    /// Bump to reimport unchanged files after normalization changes.
    fn normalization_version(&self) -> NonZeroU32 {
        NonZeroU32::MIN
    }

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<SessionData, Self::Error>;
}

pub struct FileSessionSource<D, P> {
    discovery: D,
    parser: P,
}

impl<D, P> FileSessionSource<D, P> {
    pub fn new(discovery: D, parser: P) -> Self {
        Self { discovery, parser }
    }
}

impl<D: SessionFileDiscovery, P: SessionParser> SessionSource for FileSessionSource<D, P> {
    type Error = FileSourceError;

    fn agent_id(&self) -> AgentId {
        self.discovery.agent_id()
    }

    fn normalization_version(&self) -> NonZeroU32 {
        self.parser.normalization_version()
    }

    fn discover(&self, known: &[SourceState]) -> Result<DiscoveryReport, Self::Error> {
        let report = self
            .discovery
            .discover()
            .map_err(|error| FileSourceError::Discovery(Box::new(error)))?;
        let sources: Vec<_> = report
            .files
            .into_iter()
            .map(|file| DiscoveredSource {
                key: file_source_key(&file.path),
                path: Some(file.path),
                revision: file.revision.into(),
            })
            .collect();
        let present: HashSet<_> = sources.iter().map(|source| &source.key).collect();
        let missing_sources = known
            .iter()
            .filter(|state| {
                !present.contains(&state.key)
                    && state
                        .path
                        .as_deref()
                        .is_some_and(|path| report.coverage.covers(path))
            })
            .map(|state| state.key.clone())
            .collect();
        Ok(DiscoveryReport {
            sources,
            missing_sources,
            warnings: report.warnings,
        })
    }

    fn load(&self, source: &DiscoveredSource) -> Result<SessionSnapshot, Self::Error> {
        let path = source
            .path
            .as_deref()
            .ok_or_else(|| FileSourceError::Io(io::Error::other("file source has no path")))?;
        load_stable_session(path, &self.parser)
    }
}

/// Normalizes path components without losing non-UTF-8 bytes.
pub fn file_source_key(path: &Path) -> SourceKey {
    use std::os::unix::ffi::OsStrExt;
    let normalized: PathBuf = path.components().collect();
    SourceKey(normalized.as_os_str().as_bytes().to_vec())
}

impl From<FileRevision> for SourceRevision {
    fn from(revision: FileRevision) -> Self {
        let nanos = match revision.modified_at.duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_nanos() as i128,
            Err(error) => -(error.duration().as_nanos() as i128),
        };
        Self(format!("{}:{nanos}", revision.size).into_bytes())
    }
}

impl<D: SessionFileDiscovery + ?Sized> SessionFileDiscovery for &D {
    type Error = D::Error;
    fn agent_id(&self) -> AgentId {
        (**self).agent_id()
    }
    fn discover(&self) -> Result<FileDiscoveryReport, Self::Error> {
        (**self).discover()
    }
}

impl<P: SessionParser + ?Sized> SessionParser for &P {
    type Error = P::Error;
    fn normalization_version(&self) -> NonZeroU32 {
        (**self).normalization_version()
    }
    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<SessionData, Self::Error> {
        (**self).parse(input, context)
    }
}

fn load_stable_session<P: SessionParser>(
    path: &Path,
    parser: &P,
) -> Result<SessionSnapshot, FileSourceError> {
    let mut last_retry = FileSourceError::ChangedDuringRead;

    for _ in 0..STABLE_READ_ATTEMPTS {
        match load_session_once(path, parser) {
            Err(error @ (FileSourceError::Io(_) | FileSourceError::ChangedDuringRead)) => {
                last_retry = error;
            }
            result => return result,
        }
    }

    Err(last_retry)
}

fn load_session_once<P: SessionParser>(
    path: &Path,
    parser: &P,
) -> Result<SessionSnapshot, FileSourceError> {
    let file = File::open(path)?;
    let handle_before = file.metadata()?;
    let path_before = fs::metadata(path)?;
    if !same_file_and_revision(&handle_before, &path_before) || !path_before.is_file() {
        return Err(FileSourceError::ChangedDuringRead);
    }

    let revision = FileRevision {
        size: handle_before.len(),
        modified_at: handle_before.modified()?,
    };
    let mut reader = BufReader::new(file);
    let parsed = parser
        .parse(&mut reader, ParseContext { source_path: path })
        .map_err(|error| FileSourceError::Parse(error.to_string()));

    let handle_after = reader.get_ref().metadata()?;
    let path_after = fs::metadata(path)?;
    if !same_file_and_revision(&handle_before, &handle_after)
        || !same_file_and_revision(&handle_before, &path_after)
    {
        return Err(FileSourceError::ChangedDuringRead);
    }

    Ok(SessionSnapshot {
        session: parsed?,
        revision: revision.into(),
    })
}

fn same_file_and_revision(left: &Metadata, right: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
        && left.dev() == right.dev()
        && left.ino() == right.ino()
}

#[derive(Debug)]
pub enum FileSourceError {
    Discovery(Box<dyn Error + Send + Sync>),
    Io(io::Error),
    ChangedDuringRead,
    Parse(String),
}

impl From<io::Error> for FileSourceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for FileSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Discovery(source) => fmt::Display::fmt(source, formatter),
            Self::Io(source) => write!(formatter, "could not read session file: {source}"),
            Self::ChangedDuringRead => {
                formatter.write_str("session file kept changing while being read; import deferred")
            }
            Self::Parse(source) => write!(formatter, "could not parse session file: {source}"),
        }
    }
}

impl Error for FileSourceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Discovery(source) => Some(source.as_ref()),
            Self::Io(source) => Some(source),
            _ => None,
        }
    }
}
