//! Pi version 3 JSONL session parser.

use std::error::Error;
use std::fmt;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::str;

use chrono::DateTime;
use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::application::{ParseCompletion, ParsedSession, SessionParser};
use crate::core::{
    AgentId, InvalidRecordedCost, ModelAttribution, RecordedCost, SessionMetadata, Timestamp,
    TokenCounts, UsageEvent, UsageEventIdentity, UsageKind,
};

const PI_AGENT_ID: &str = "pi";
const SUPPORTED_SESSION_VERSION: u32 = 3;

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
