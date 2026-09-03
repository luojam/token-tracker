//! Pi session discovery and version 3 JSONL parsing.

use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::str;

use chrono::DateTime;
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::application::{
    DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, DiscoveryWarning, FileRevision,
    ParseCompletion, ParsedSession, SessionDiscovery, SessionParser,
};
use crate::core::{
    AgentId, InvalidRecordedCost, ModelAttribution, RecordedCost, SessionMetadata, Timestamp,
    TokenCounts, UsageEvent, UsageEventIdentity, UsageKind,
};

const PI_AGENT_ID: &str = "pi";
const SUPPORTED_SESSION_VERSION: u32 = 3;
const PI_AGENT_DIRECTORY_ENV: &str = "PI_CODING_AGENT_DIR";
const PI_SESSION_DIRECTORY_ENV: &str = "PI_CODING_AGENT_SESSION_DIR";
const PI_CONFIG_DIRECTORY: &str = ".pi";
const PI_AGENT_DIRECTORY: &str = "agent";
const PI_SESSIONS_DIRECTORY: &str = "sessions";
const SESSION_EXTENSION: &str = "jsonl";

/// Resolves Pi's session root, including its session and agent directory overrides.
pub fn default_session_root() -> Result<PathBuf, PiDiscoveryError> {
    default_session_root_from(
        env::var_os(PI_SESSION_DIRECTORY_ENV).as_deref(),
        env::var_os(PI_AGENT_DIRECTORY_ENV).as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn default_session_root_from(
    session_directory: Option<&OsStr>,
    agent_directory: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, PiDiscoveryError> {
    let home_directory = || {
        home.filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or(PiDiscoveryError::HomeDirectoryUnavailable)
    };
    let resolve_override = |directory: &OsStr| {
        let directory = PathBuf::from(directory);
        match directory.strip_prefix("~") {
            Ok(relative) => home_directory().map(|home| home.join(relative)),
            Err(_) => Ok(directory),
        }
    };

    if let Some(directory) = session_directory.filter(|directory| !directory.is_empty()) {
        return resolve_override(directory);
    }

    let agent_directory = match agent_directory.filter(|directory| !directory.is_empty()) {
        Some(directory) => resolve_override(directory)?,
        None => home_directory()?
            .join(PI_CONFIG_DIRECTORY)
            .join(PI_AGENT_DIRECTORY),
    };

    Ok(agent_directory.join(PI_SESSIONS_DIRECTORY))
}

/// Recursively discovers Pi session JSONL files without reading their contents.
#[derive(Clone, Debug)]
pub struct PiSessionDiscovery {
    root: PathBuf,
}

impl PiSessionDiscovery {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn for_default_root() -> Result<Self, PiDiscoveryError> {
        default_session_root().map(Self::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SessionDiscovery for PiSessionDiscovery {
    type Error = PiDiscoveryError;

    fn discover(&self) -> Result<DiscoveryReport, Self::Error> {
        if self.root.as_os_str().is_empty() {
            return Err(PiDiscoveryError::EmptySessionRoot);
        }
        let root = std::path::absolute(&self.root)
            .map_err(|source| PiDiscoveryError::SessionRootResolution { source })?;

        let mut report = DiscoveryReport {
            files: Vec::new(),
            warnings: Vec::new(),
            coverage: DiscoveryCoverage {
                inspected_roots: vec![root.clone()],
                inaccessible_paths: Vec::new(),
            },
        };
        let mut pending_directories = vec![root];

        while let Some(directory) = pending_directories.pop() {
            scan_directory(&directory, &mut pending_directories, &mut report);
        }

        report
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        report.coverage.inaccessible_paths.sort_unstable();
        report.coverage.inaccessible_paths.dedup();
        report.warnings.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.message.cmp(&right.message))
        });
        Ok(report)
    }
}

fn scan_directory(
    directory: &Path,
    pending_directories: &mut Vec<PathBuf>,
    report: &mut DiscoveryReport,
) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            record_inaccessible(
                report,
                directory,
                format!("could not read directory: {error}"),
            );
            return;
        }
    };

    let mut readable_entries = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => readable_entries.push(entry),
            Err(error) => record_inaccessible(
                report,
                directory,
                format!("could not read directory entry: {error}"),
            ),
        }
    }
    readable_entries.sort_by_key(fs::DirEntry::path);

    for entry in readable_entries {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                record_inaccessible(report, &path, format!("could not inspect path: {error}"));
                continue;
            }
        };

        if file_type.is_dir() {
            pending_directories.push(path);
            continue;
        }
        if path.extension() != Some(OsStr::new(SESSION_EXTENSION)) {
            continue;
        }

        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                record_inaccessible(
                    report,
                    &path,
                    format!("could not read file metadata: {error}"),
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let modified_at = match metadata.modified() {
            Ok(modified_at) => modified_at,
            Err(error) => {
                record_inaccessible(
                    report,
                    &path,
                    format!("could not read modification time: {error}"),
                );
                continue;
            }
        };

        report.files.push(DiscoveredSessionFile {
            path,
            revision: FileRevision {
                size: metadata.len(),
                modified_at,
            },
        });
    }
}

fn record_inaccessible(report: &mut DiscoveryReport, path: &Path, message: String) {
    let path = path.to_owned();
    report.coverage.inaccessible_paths.push(path.clone());
    report.warnings.push(DiscoveryWarning {
        path: Some(path),
        message,
    });
}

#[derive(Debug)]
pub enum PiDiscoveryError {
    EmptySessionRoot,
    HomeDirectoryUnavailable,
    SessionRootResolution { source: io::Error },
}

impl fmt::Display for PiDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySessionRoot => formatter.write_str("Pi session root is empty"),
            Self::HomeDirectoryUnavailable => {
                formatter.write_str("HOME is unavailable or is not an absolute path")
            }
            Self::SessionRootResolution { source } => {
                write!(formatter, "could not resolve Pi session root: {source}")
            }
        }
    }
}

impl Error for PiDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SessionRootResolution { source } => Some(source),
            Self::EmptySessionRoot | Self::HomeDirectoryUnavailable => None,
        }
    }
}

/// Parses current Pi session files without projecting message content into the result.
#[derive(Clone, Copy, Debug, Default)]
pub struct PiSessionParser;

impl PiSessionParser {
    pub const fn new() -> Self {
        Self
    }
}

impl SessionParser for PiSessionParser {
    type Error = PiParseError;

    fn parse(&self, input: &mut dyn BufRead) -> Result<ParsedSession, Self::Error> {
        let mut line = Vec::new();
        let bytes_read = input
            .read_until(b'\n', &mut line)
            .map_err(|source| PiParseError::Io { line: 1, source })?;

        if bytes_read == 0 {
            return Err(PiParseError::MissingHeader);
        }

        let terminated = line.ends_with(b"\n");
        let header_line = match str::from_utf8(&line) {
            Ok(line) => line,
            Err(source) if !terminated && source.error_len().is_none() => {
                return Err(PiParseError::IncompleteHeader);
            }
            Err(_) => return Err(PiParseError::InvalidUtf8 { line: 1 }),
        };
        let header: HeaderWire = match serde_json::from_str(header_line) {
            Ok(header) => header,
            Err(source) if !terminated && source.is_eof() => {
                return Err(PiParseError::IncompleteHeader);
            }
            Err(_) => return Err(PiParseError::MalformedLine { line: 1 }),
        };

        if header.entry_type != "session" {
            return Err(PiParseError::InvalidHeader);
        }

        let version = header.version.unwrap_or(1);
        if version != SUPPORTED_SESSION_VERSION {
            return Err(PiParseError::UnsupportedVersion(version));
        }

        if header.id.is_empty() {
            return Err(PiParseError::InvalidField {
                line: 1,
                field: "session id",
            });
        }

        let started_at = parse_timestamp(&header.timestamp)
            .map_err(|source| PiParseError::InvalidTimestamp { line: 1, source })?;

        let mut metadata = SessionMetadata {
            agent: AgentId::from(PI_AGENT_ID),
            session_id: header.id,
            format_version: version,
            working_directory: PathBuf::from(header.cwd),
            started_at,
            name: None,
            parent_session: header.parent_session,
        };
        let mut events = Vec::new();
        let mut line_number = 1;
        let mut completion = ParseCompletion::Complete;

        loop {
            line.clear();
            line_number += 1;
            let bytes_read =
                input
                    .read_until(b'\n', &mut line)
                    .map_err(|source| PiParseError::Io {
                        line: line_number,
                        source,
                    })?;

            if bytes_read == 0 {
                break;
            }

            let terminated = line.ends_with(b"\n");
            let entry_line = match str::from_utf8(&line) {
                Ok(line) => line,
                Err(source) if !terminated && source.error_len().is_none() => {
                    completion = ParseCompletion::IncompleteFinalLine;
                    break;
                }
                Err(_) => return Err(PiParseError::InvalidUtf8 { line: line_number }),
            };

            match parse_entry_line(entry_line, &mut metadata, &mut events) {
                Ok(()) => {}
                Err(EntryError::Json(source)) if !terminated && source.is_eof() => {
                    completion = ParseCompletion::IncompleteFinalLine;
                    break;
                }
                Err(error) => return Err(error.into_parse_error(line_number)),
            }
        }

        Ok(ParsedSession {
            metadata,
            events,
            completion,
        })
    }
}

#[derive(Debug)]
pub enum PiParseError {
    MissingHeader,
    IncompleteHeader,
    InvalidHeader,
    UnsupportedVersion(u32),
    InvalidField {
        line: usize,
        field: &'static str,
    },
    MalformedLine {
        line: usize,
    },
    InvalidUtf8 {
        line: usize,
    },
    InvalidTimestamp {
        line: usize,
        source: chrono::ParseError,
    },
    InvalidRecordedCost {
        line: usize,
        source: InvalidRecordedCost,
    },
    Io {
        line: usize,
        source: io::Error,
    },
}

impl fmt::Display for PiParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => formatter.write_str("Pi session is missing its header"),
            Self::IncompleteHeader => formatter.write_str("Pi session header is incomplete"),
            Self::InvalidHeader => {
                formatter.write_str("first JSONL value is not a Pi session header")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported Pi session version {version}")
            }
            Self::InvalidField { line, field } => {
                write!(formatter, "invalid {field} on Pi session line {line}")
            }
            Self::MalformedLine { line } => write!(formatter, "malformed Pi session line {line}"),
            Self::InvalidUtf8 { line } => {
                write!(formatter, "invalid UTF-8 on Pi session line {line}")
            }
            Self::InvalidTimestamp { line, source } => {
                write!(
                    formatter,
                    "invalid timestamp on Pi session line {line}: {source}"
                )
            }
            Self::InvalidRecordedCost { line, source } => {
                write!(
                    formatter,
                    "invalid cost on Pi session line {line}: {source}"
                )
            }
            Self::Io { line, source } => {
                write!(formatter, "could not read Pi session line {line}: {source}")
            }
        }
    }
}

impl Error for PiParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidTimestamp { source, .. } => Some(source),
            Self::InvalidRecordedCost { source, .. } => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::MissingHeader
            | Self::IncompleteHeader
            | Self::InvalidHeader
            | Self::UnsupportedVersion(_)
            | Self::InvalidField { .. }
            | Self::MalformedLine { .. }
            | Self::InvalidUtf8 { .. } => None,
        }
    }
}

fn parse_entry_line(
    line: &str,
    metadata: &mut SessionMetadata,
    events: &mut Vec<UsageEvent>,
) -> Result<(), EntryError> {
    let entry: EntryTypeWire = serde_json::from_str(line)?;

    match entry.entry_type.as_str() {
        "message" => parse_message_entry(line, events),
        "compaction" => parse_summary_entry(line, UsageKind::Compaction, events),
        "branch_summary" => parse_summary_entry(line, UsageKind::BranchSummary, events),
        "session_info" => {
            let session_info: SessionInfoWire = serde_json::from_str(line)?;
            metadata.name = session_info
                .name
                .map(|name| name.trim().to_owned())
                .filter(|name| !name.is_empty());
            Ok(())
        }
        _ => Ok(()),
    }
}

fn parse_message_entry(line: &str, events: &mut Vec<UsageEvent>) -> Result<(), EntryError> {
    let discriminator: MessageDiscriminatorWire = serde_json::from_str(line)?;
    if discriminator.message.usage.is_none() {
        return Ok(());
    }

    match discriminator.message.role.as_str() {
        "assistant" => {
            let entry: AssistantUsageEntryWire = serde_json::from_str(line)?;
            events.push(normalize_event(
                entry.id,
                entry.timestamp,
                UsageKind::Assistant,
                Some(ModelAttribution {
                    provider: entry.message.provider,
                    model: entry.message.response_model.unwrap_or(entry.message.model),
                }),
                entry.message.usage,
            )?);
        }
        "toolResult" => {
            let entry: ToolUsageEntryWire = serde_json::from_str(line)?;
            events.push(normalize_event(
                entry.id,
                entry.timestamp,
                UsageKind::ToolResult,
                None,
                entry.message.usage,
            )?);
        }
        _ => {}
    }

    Ok(())
}

fn parse_summary_entry(
    line: &str,
    kind: UsageKind,
    events: &mut Vec<UsageEvent>,
) -> Result<(), EntryError> {
    let discriminator: EntryUsageDiscriminatorWire = serde_json::from_str(line)?;
    if discriminator.usage.is_none() {
        return Ok(());
    }

    let entry: SummaryUsageEntryWire = serde_json::from_str(line)?;
    events.push(normalize_event(
        entry.id,
        entry.timestamp,
        kind,
        None,
        entry.usage,
    )?);
    Ok(())
}

fn normalize_event(
    entry_id: String,
    timestamp: String,
    kind: UsageKind,
    attribution: Option<ModelAttribution>,
    usage: UsageWire,
) -> Result<UsageEvent, EntryError> {
    if entry_id.is_empty() {
        return Err(EntryError::InvalidField("entry id"));
    }

    let timestamp = parse_timestamp(&timestamp)?;
    let recorded_cost = usage
        .cost
        .and_then(|cost| cost.total)
        .map(RecordedCost::from_usd)
        .transpose()?;

    Ok(UsageEvent {
        identity: UsageEventIdentity {
            agent: AgentId::from(PI_AGENT_ID),
            adapter_key: adapter_key(&entry_id, timestamp, kind),
        },
        timestamp,
        kind,
        attribution,
        tokens: TokenCounts {
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
        },
        recorded_cost,
    })
}

/// Versioned key made only from immutable Pi entry fields. Copied fork/clone
/// entries therefore retain the same key even when their source or usage changes.
fn adapter_key(entry_id: &str, timestamp: Timestamp, kind: UsageKind) -> String {
    let kind = match kind {
        UsageKind::Assistant => "assistant",
        UsageKind::ToolResult => "tool-result",
        UsageKind::Compaction => "compaction",
        UsageKind::BranchSummary => "branch-summary",
    };

    format!("v1:{kind}:{}:{entry_id}", timestamp.as_unix_milliseconds())
}

fn parse_timestamp(value: &str) -> Result<Timestamp, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| Timestamp::from_unix_milliseconds(timestamp.timestamp_millis()))
}

#[derive(Deserialize)]
struct HeaderWire {
    #[serde(rename = "type")]
    entry_type: String,
    version: Option<u32>,
    id: String,
    timestamp: String,
    cwd: String,
    #[serde(rename = "parentSession")]
    parent_session: Option<String>,
}

#[derive(Deserialize)]
struct EntryTypeWire {
    #[serde(rename = "type")]
    entry_type: String,
}

#[derive(Deserialize)]
struct MessageDiscriminatorWire {
    message: MessageDiscriminator,
}

#[derive(Deserialize)]
struct MessageDiscriminator {
    role: String,
    #[serde(default)]
    usage: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct EntryUsageDiscriminatorWire {
    #[serde(default)]
    usage: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct AssistantUsageEntryWire {
    id: String,
    timestamp: String,
    message: AssistantUsageWire,
}

#[derive(Deserialize)]
struct AssistantUsageWire {
    provider: String,
    model: String,
    #[serde(rename = "responseModel")]
    response_model: Option<String>,
    usage: UsageWire,
}

#[derive(Deserialize)]
struct ToolUsageEntryWire {
    id: String,
    timestamp: String,
    message: ToolUsageWire,
}

#[derive(Deserialize)]
struct ToolUsageWire {
    usage: UsageWire,
}

#[derive(Deserialize)]
struct SummaryUsageEntryWire {
    id: String,
    timestamp: String,
    usage: UsageWire,
}

#[derive(Deserialize)]
struct SessionInfoWire {
    name: Option<String>,
}

#[derive(Deserialize)]
struct UsageWire {
    input: u64,
    output: u64,
    #[serde(rename = "cacheRead")]
    cache_read: u64,
    #[serde(rename = "cacheWrite")]
    cache_write: u64,
    cost: Option<CostWire>,
}

#[derive(Deserialize)]
struct CostWire {
    total: Option<f64>,
}

#[derive(Debug)]
enum EntryError {
    Json(serde_json::Error),
    InvalidTimestamp(chrono::ParseError),
    InvalidRecordedCost(InvalidRecordedCost),
    InvalidField(&'static str),
}

impl EntryError {
    fn into_parse_error(self, line: usize) -> PiParseError {
        match self {
            Self::Json(_) => PiParseError::MalformedLine { line },
            Self::InvalidTimestamp(source) => PiParseError::InvalidTimestamp { line, source },
            Self::InvalidRecordedCost(source) => PiParseError::InvalidRecordedCost { line, source },
            Self::InvalidField(field) => PiParseError::InvalidField { line, field },
        }
    }
}

impl From<serde_json::Error> for EntryError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}

impl From<chrono::ParseError> for EntryError {
    fn from(source: chrono::ParseError) -> Self {
        Self::InvalidTimestamp(source)
    }
}

impl From<InvalidRecordedCost> for EntryError {
    fn from(source: InvalidRecordedCost) -> Self {
        Self::InvalidRecordedCost(source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_TREE: AtomicU64 = AtomicU64::new(0);

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new() -> Self {
            Self::new_in(&env::temp_dir())
        }

        fn new_in(parent: &Path) -> Self {
            let sequence = NEXT_TEMP_TREE.fetch_add(1, Ordering::Relaxed);
            let root = parent.join(format!(
                "token-tracker-pi-discovery-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            Self { root }
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn default_root_honors_the_directory_overrides() {
        let home = env::temp_dir().join("token-tracker-home");
        assert_eq!(
            default_session_root_from(None, None, Some(home.as_os_str())).unwrap(),
            home.join(".pi/agent/sessions")
        );

        let custom_agent_directory = home.join("custom-agent");
        assert_eq!(
            default_session_root_from(None, Some(custom_agent_directory.as_os_str()), None,)
                .unwrap(),
            custom_agent_directory.join("sessions")
        );
        assert_eq!(
            default_session_root_from(
                None,
                Some(OsStr::new("~/custom-agent")),
                Some(home.as_os_str()),
            )
            .unwrap(),
            home.join("custom-agent/sessions")
        );

        let custom_session_directory = home.join("current-sessions");
        assert_eq!(
            default_session_root_from(
                Some(custom_session_directory.as_os_str()),
                Some(custom_agent_directory.as_os_str()),
                None,
            )
            .unwrap(),
            custom_session_directory
        );
        assert_eq!(
            default_session_root_from(
                Some(OsStr::new("~/current-sessions")),
                None,
                Some(home.as_os_str()),
            )
            .unwrap(),
            home.join("current-sessions")
        );
        assert!(matches!(
            default_session_root_from(None, None, Some(OsStr::new("relative-home"))),
            Err(PiDiscoveryError::HomeDirectoryUnavailable)
        ));
    }

    #[test]
    fn discovery_rejects_an_empty_root() {
        assert!(matches!(
            PiSessionDiscovery::new("").discover(),
            Err(PiDiscoveryError::EmptySessionRoot)
        ));
    }

    #[test]
    fn discovery_resolves_relative_roots_to_absolute_paths() {
        let current_directory = env::current_dir().unwrap();
        let tree = TempTree::new_in(&current_directory);
        let relative_root = tree.root.strip_prefix(&current_directory).unwrap();
        let session = tree.root.join("session.jsonl");
        fs::write(&session, b"session").unwrap();

        let report = PiSessionDiscovery::new(relative_root).discover().unwrap();

        assert_eq!(report.coverage.inspected_roots, vec![tree.root.clone()]);
        assert_eq!(report.files[0].path, session);
    }

    #[test]
    fn discovery_recurses_and_returns_file_revisions_in_path_order() {
        let tree = TempTree::new();
        let project = tree.root.join("project");
        let nested = project.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let first = project.join("a.jsonl");
        let second = nested.join("b.jsonl");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second session").unwrap();
        fs::write(project.join("ignored.txt"), b"not a session").unwrap();
        fs::write(project.join("ignored.JSONL"), b"not a Pi session").unwrap();

        let report = PiSessionDiscovery::new(&tree.root).discover().unwrap();

        assert!(report.warnings.is_empty());
        assert_eq!(report.coverage.inspected_roots, vec![tree.root.clone()]);
        assert!(report.coverage.inaccessible_paths.is_empty());
        assert_eq!(
            report
                .files
                .iter()
                .map(|file| (&file.path, file.revision.size))
                .collect::<Vec<_>>(),
            vec![(&first, 5), (&second, 14)]
        );
        assert_eq!(
            report.files[0].revision.modified_at,
            fs::metadata(&first).unwrap().modified().unwrap()
        );
        assert_eq!(
            report.files[1].revision.modified_at,
            fs::metadata(&second).unwrap().modified().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn discovery_warns_about_an_unreadable_candidate_and_continues() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let valid = tree.root.join("valid.jsonl");
        let inaccessible = tree.root.join("missing.jsonl");
        fs::write(&valid, b"session").unwrap();
        symlink(tree.root.join("missing-target"), &inaccessible).unwrap();

        let report = PiSessionDiscovery::new(&tree.root).discover().unwrap();

        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].path, valid);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            report.warnings[0].path.as_deref(),
            Some(inaccessible.as_path())
        );
        assert!(
            report.warnings[0]
                .message
                .starts_with("could not read file metadata:")
        );
        assert_eq!(report.coverage.inaccessible_paths, vec![inaccessible]);
    }
}
