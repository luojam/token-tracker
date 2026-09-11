use super::CLAUDE_AGENT_ID;
use super::discovery::{is_agent_id, is_session_id};
use crate::adapters::jsonl::{JsonlError, JsonlLine, JsonlReader};
use crate::application::{
    ParseCompletion, ParseContext, ParseNotice, ParsedSession, SessionParser,
};
use crate::domain::{
    AgentId, CacheDetail, CacheWriteTokens, ModelAttribution, ParentSession, PricingContext,
    RequestGranularity, ServiceTier, SessionMetadata, TierEvidence, Timestamp, TokenCounts,
    UsageEvent, UsageEventIdentity, UsageKind,
};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{self, BufRead};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::{error::Error, ffi::OsStr, fmt};

#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudeSessionParser;

impl ClaudeSessionParser {
    pub const fn new() -> Self {
        Self
    }
}

impl SessionParser for ClaudeSessionParser {
    type Error = ClaudeParseError;

    fn normalization_version(&self) -> u32 {
        super::NORMALIZATION_VERSION
    }

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error> {
        let mut metadata = Metadata::from_path(context.source_path)?;
        let mut responses = BTreeMap::<String, Response>::new();
        let mut notices = Vec::new();
        let mut completion = ParseCompletion::Complete;
        let mut lines = JsonlReader::new(input);
        loop {
            let (line, text) = match lines.next_line()? {
                JsonlLine::Complete { number, text } => (number, text),
                JsonlLine::Incomplete { number } => {
                    completion = ParseCompletion::IncompleteFinalLine;
                    add_notice(&mut notices, ParseNoticeCode::TruncatedTail, number)?;
                    break;
                }
                JsonlLine::Eof => break,
            };
            let record: Record = serde_json::from_str(text).map_err(|_| invalid(line, "record"))?;
            let timestamp = metadata.observe(&record, line)?;
            if record.entry_type.as_str() != Some("assistant") {
                continue;
            }
            let assistant: Assistant =
                serde_json::from_str(text).map_err(|_| invalid(line, "assistant message"))?;
            observe_response(assistant, timestamp, line, &mut responses)?;
        }

        let mut events = Vec::new();
        for (id, response) in responses {
            match response.snapshot {
                Some((tokens, pricing_context)) => events.push(UsageEvent {
                    identity: UsageEventIdentity {
                        agent: AgentId::from(CLAUDE_AGENT_ID),
                        adapter_key: format!("response-v1:{id}"),
                    },
                    timestamp: response
                        .first_final
                        .expect("final snapshot has a timestamp"),
                    kind: UsageKind::Assistant,
                    attribution: response.model.map(|model| ModelAttribution {
                        provider: if model.starts_with("claude-") {
                            "anthropic"
                        } else {
                            "unknown"
                        }
                        .into(),
                        model,
                    }),
                    tokens,
                    recorded_cost: None,
                    pricing_context: Some(pricing_context),
                }),
                None => add_notice(
                    &mut notices,
                    if response.first_final.is_some() {
                        ParseNoticeCode::UnsupportedResponseAccounting
                    } else {
                        ParseNoticeCode::IncompleteResponseUsage
                    },
                    response.omission_line,
                )?,
            }
        }
        Ok(ParsedSession {
            metadata: metadata.finish()?,
            events,
            completion,
            notices,
        })
    }
}

#[derive(Deserialize)]
struct Record {
    #[serde(default, rename = "type")]
    entry_type: Value,
    #[serde(default, rename = "sessionId")]
    session_id: Value,
    #[serde(default, rename = "agentId")]
    agent_id: Value,
    #[serde(default)]
    timestamp: Value,
    #[serde(default)]
    cwd: Value,
}

#[derive(Deserialize)]
struct Assistant {
    message: Message,
    #[serde(default, rename = "requestId")]
    request_id: Value,
}

#[derive(Deserialize)]
struct Message {
    id: String,
    #[serde(default)]
    model: Value,
    #[serde(default)]
    stop_reason: Value,
    usage: Value,
}

struct Metadata {
    session_id: String,
    agent_id: Option<String>,
    seen_session: bool,
    seen_agent: bool,
    started_at: Option<Timestamp>,
    cwd: Option<(Option<Timestamp>, PathBuf)>,
}

impl Metadata {
    fn from_path(path: &Path) -> Result<Self, ClaudeParseError> {
        if !path.is_absolute() || path.extension() != Some(OsStr::new("jsonl")) {
            return Err(ClaudeParseError::InvalidSourcePath);
        }
        let stem = path
            .file_stem()
            .ok_or(ClaudeParseError::InvalidSourcePath)?;
        let (session_id, agent_id) = if is_session_id(stem) {
            (stem.to_str().unwrap().to_owned(), None)
        } else {
            let agent = stem
                .to_str()
                .and_then(|stem| stem.strip_prefix("agent-"))
                .filter(|agent| is_agent_id(agent))
                .ok_or(ClaudeParseError::InvalidSourcePath)?;
            let session = path
                .ancestors()
                .skip(1)
                .filter(|parent| parent.file_name() == Some(OsStr::new("subagents")))
                .filter_map(|parent| parent.parent().and_then(Path::file_name))
                .find(|name| is_session_id(name))
                .ok_or(ClaudeParseError::InvalidSourcePath)?;
            (session.to_str().unwrap().to_owned(), Some(agent.to_owned()))
        };
        Ok(Self {
            session_id,
            agent_id,
            seen_session: false,
            seen_agent: false,
            started_at: None,
            cwd: None,
        })
    }

    fn observe(
        &mut self,
        record: &Record,
        line: usize,
    ) -> Result<Option<Timestamp>, ClaudeParseError> {
        if !record.session_id.is_null() {
            if record.session_id.as_str() != Some(&self.session_id) {
                return Err(invalid(line, "session identity"));
            }
            self.seen_session = true;
        }
        if !record.agent_id.is_null() {
            if record.agent_id.as_str().is_none()
                || record.agent_id.as_str() != self.agent_id.as_deref()
            {
                return Err(invalid(line, "agent identity"));
            }
            self.seen_agent = true;
        }
        let timestamp = record
            .timestamp
            .as_str()
            .map(|timestamp| {
                DateTime::parse_from_rfc3339(timestamp)
                    .map(|timestamp| {
                        Timestamp::from_unix_milliseconds(timestamp.timestamp_millis())
                    })
                    .map_err(|_| invalid(line, "timestamp"))
            })
            .transpose()?;
        if let Some(timestamp) = timestamp {
            self.started_at = Some(self.started_at.map_or(timestamp, |old| old.min(timestamp)));
        }
        if let Some(cwd) = record.cwd.as_str().filter(|cwd| !cwd.is_empty())
            && self.cwd.as_ref().is_none_or(|(old, _)| {
                timestamp.is_some_and(|timestamp| old.is_none_or(|old| timestamp < old))
            })
        {
            self.cwd = Some((timestamp, cwd.into()));
        }
        Ok(timestamp)
    }

    fn finish(self) -> Result<SessionMetadata, ClaudeParseError> {
        if !self.seen_session {
            return Err(ClaudeParseError::MissingMetadata { field: "sessionId" });
        }
        if self.agent_id.is_some() && !self.seen_agent {
            return Err(ClaudeParseError::MissingMetadata { field: "agentId" });
        }
        let started_at = self
            .started_at
            .ok_or(ClaudeParseError::MissingMetadata { field: "timestamp" })?;
        let (session_id, parent_session) = match self.agent_id {
            Some(agent) => (
                format!("subagent-v1:{}:{agent}", self.session_id),
                Some(ParentSession::SessionId(self.session_id)),
            ),
            None => (self.session_id, None),
        };
        Ok(SessionMetadata {
            agent: AgentId::from(CLAUDE_AGENT_ID),
            session_id,

            working_directory: self.cwd.map(|(_, cwd)| cwd),
            started_at,
            name: None,
            parent_session,
        })
    }
}

struct Response {
    model: Option<String>,
    request_id: Option<String>,
    first_final: Option<Timestamp>,
    snapshot: Option<(TokenCounts, PricingContext)>,
    pending_final_usage: Option<(Value, usize)>,
    omission_line: usize,
}

fn observe_response(
    assistant: Assistant,
    timestamp: Option<Timestamp>,
    line: usize,
    responses: &mut BTreeMap<String, Response>,
) -> Result<(), ClaudeParseError> {
    let message = assistant.message;
    if message.id.trim().is_empty() {
        return Err(invalid(line, "response id"));
    }
    let model = optional_string(&message.model, line, "model")?;
    let request_id = optional_string(&assistant.request_id, line, "request id")?;
    let final_snapshot = optional_string(&message.stop_reason, line, "stop reason")?.is_some();
    let response_model = model.or_else(|| {
        responses
            .get(&message.id)
            .and_then(|response| response.model.as_deref())
    });
    let accounting = parse_usage(&message.usage, response_model, line)?;
    if model == Some("<synthetic>")
        && accounting
            .as_ref()
            .is_some_and(|(tokens, _)| tokens.total() == 0)
    {
        return Ok(());
    }
    let response = responses.entry(message.id).or_insert_with(|| Response {
        model: None,
        request_id: None,
        first_final: None,
        snapshot: None,
        pending_final_usage: None,
        omission_line: line,
    });
    for (old, new, field) in [
        (&mut response.model, model, "conflicting model"),
        (
            &mut response.request_id,
            request_id,
            "conflicting request id",
        ),
    ] {
        if let Some(new) = new {
            if old.as_deref().is_some_and(|old| old != new) {
                return Err(invalid(line, field));
            }
            *old = Some(new.to_owned());
        }
    }
    if final_snapshot {
        let timestamp = timestamp.ok_or_else(|| invalid(line, "response timestamp"))?;
        if response.first_final.is_none() || response.snapshot.is_some() {
            response.omission_line = line;
        }
        response.first_final.get_or_insert(timestamp);
        response.pending_final_usage = if accounting.is_none() && response.model.is_none() {
            Some((message.usage, line))
        } else {
            None
        };
        response.snapshot = accounting;
    } else if response.model.is_some()
        && let Some((usage, final_line)) = response.pending_final_usage.take()
    {
        response.snapshot = parse_usage(&usage, response.model.as_deref(), final_line)?;
    }
    Ok(())
}

fn optional_string<'a>(
    value: &'a Value,
    line: usize,
    field: &'static str,
) -> Result<Option<&'a str>, ClaudeParseError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok((!value.is_empty()).then_some(value.as_str())),
        _ => Err(invalid(line, field)),
    }
}

fn parse_usage(
    usage: &Value,
    model: Option<&str>,
    line: usize,
) -> Result<Option<(TokenCounts, PricingContext)>, ClaudeParseError> {
    let top = component(usage, line)?;
    let (tokens, components) = match usage.get("iterations").filter(|value| !value.is_null()) {
        None => (top.tokens, vec![top]),
        Some(value) => {
            let Some(iterations) = value.as_array().filter(|items| !items.is_empty()) else {
                return Ok(None);
            };
            let mut supported = true;
            let mut components = Vec::new();
            let mut total = TokenCounts::default();
            let mut non_compaction = TokenCounts::default();
            for iteration in iterations {
                let is_message = match iteration.get("type").and_then(Value::as_str) {
                    Some("message") => true,
                    Some("compaction") => false,
                    _ => {
                        supported = false;
                        continue;
                    }
                };
                let component = component(iteration, line)?;
                if iteration
                    .get("model")
                    .is_some_and(|value| value.as_str().is_none() || value.as_str() != model)
                {
                    supported = false;
                }
                total = total
                    .checked_add(component.tokens)
                    .ok_or_else(|| invalid(line, "counter overflow"))?;
                if is_message {
                    non_compaction = non_compaction
                        .checked_add(component.tokens)
                        .ok_or_else(|| invalid(line, "counter overflow"))?;
                }
                components.push(component);
            }
            if !supported {
                return Ok(None);
            }
            if non_compaction != top.tokens {
                return Err(invalid(line, "non-compaction total mismatch"));
            }
            (total, components)
        }
    };
    let mut writes = BTreeMap::<u32, u64>::new();
    let mut complete_durations = true;
    for component in &components {
        match &component.cache_creation {
            Some(durations) => {
                for duration in durations {
                    let total = writes.entry(duration.duration_seconds).or_default();
                    *total = total
                        .checked_add(duration.tokens)
                        .ok_or_else(|| invalid(line, "cache duration overflow"))?;
                }
            }
            None if component.tokens.cache_write > 0 => complete_durations = false,
            None => {}
        }
    }
    let pricing = PricingContext {
        provider: "anthropic".into(),
        tier: served_value(usage.get("service_tier"), line, false)?,
        speed: served_value(usage.get("speed"), line, true)?,
        tier_evidence: TierEvidence::ServedResponse,
        request_granularity: RequestGranularity::ExactSingleRequest,
        cache_detail: CacheDetail::Complete,
        request_usage: Some(
            components
                .iter()
                .map(|component| component.tokens)
                .collect(),
        ),
        cache_writes: complete_durations.then(|| {
            writes
                .into_iter()
                .map(|(duration_seconds, tokens)| CacheWriteTokens {
                    duration_seconds,
                    tokens,
                })
                .collect()
        }),
    };
    Ok(Some((tokens, pricing)))
}

fn component(usage: &Value, line: usize) -> Result<UsageComponent, ClaudeParseError> {
    let tokens = TokenCounts {
        input: counter(usage, "input_tokens", line)?,
        output: counter(usage, "output_tokens", line)?,
        cache_read: counter(usage, "cache_read_input_tokens", line)?,
        cache_write: counter(usage, "cache_creation_input_tokens", line)?,
    };
    let cache_creation = usage
        .get("cache_creation")
        .filter(|value| !value.is_null())
        .map(|value| {
            let ephemeral_5m = counter(value, "ephemeral_5m_input_tokens", line)?;
            let ephemeral_1h = counter(value, "ephemeral_1h_input_tokens", line)?;
            if ephemeral_5m.checked_add(ephemeral_1h) != Some(tokens.cache_write) {
                return Err(invalid(line, "cache duration sum mismatch"));
            }
            Ok(vec![
                CacheWriteTokens {
                    duration_seconds: 300,
                    tokens: ephemeral_5m,
                },
                CacheWriteTokens {
                    duration_seconds: 3600,
                    tokens: ephemeral_1h,
                },
            ])
        })
        .transpose()?;
    Ok(UsageComponent {
        tokens,
        cache_creation,
    })
}

fn counter(usage: &Value, field: &str, line: usize) -> Result<u64, ClaudeParseError> {
    usage
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(line, "required counter"))
}

struct UsageComponent {
    tokens: TokenCounts,
    cache_creation: Option<Vec<CacheWriteTokens>>,
}

fn served_value(
    value: Option<&Value>,
    line: usize,
    allow_fast: bool,
) -> Result<ServiceTier, ClaudeParseError> {
    match value {
        None | Some(Value::Null) => Ok(ServiceTier::Unknown),
        Some(Value::String(value)) => Ok(match value.as_str() {
            "standard" => ServiceTier::Standard,
            "fast" if allow_fast => ServiceTier::Fast,
            _ => ServiceTier::Unsupported(value.clone()),
        }),
        _ => Err(invalid(line, "served pricing evidence")),
    }
}

#[derive(Clone, Copy)]
enum ParseNoticeCode {
    IncompleteResponseUsage,
    UnsupportedResponseAccounting,
    TruncatedTail,
}

impl ParseNoticeCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::IncompleteResponseUsage => "incomplete_response_usage",
            Self::UnsupportedResponseAccounting => "unsupported_response_accounting",
            Self::TruncatedTail => "truncated_tail",
        }
    }

    fn message(self, count: u64) -> String {
        let plural = if count == 1 { "" } else { "s" };
        match self {
            Self::IncompleteResponseUsage => {
                format!("omitted {count} response{plural} with incomplete usage")
            }
            Self::UnsupportedResponseAccounting => {
                format!("omitted {count} response{plural} with unsupported accounting")
            }
            Self::TruncatedTail => {
                format!("omitted {count} truncated JSON tail{plural}; usage may be missing")
            }
        }
    }
}

fn add_notice(
    notices: &mut Vec<ParseNotice>,
    code: ParseNoticeCode,
    line: usize,
) -> Result<(), ClaudeParseError> {
    let line_number = u64::try_from(line)
        .ok()
        .and_then(NonZeroU64::new)
        .ok_or_else(|| invalid(line, "line overflow"))?;
    if let Some(notice) = notices
        .iter_mut()
        .find(|notice| notice.code == code.as_str())
    {
        notice.count = notice
            .count
            .checked_add(1)
            .ok_or_else(|| invalid(line, "notice overflow"))?;
        notice.line = Some(notice.line.map_or(line_number, |old| old.min(line_number)));
        notice.message = code.message(notice.count.get());
    } else {
        notices.push(ParseNotice {
            code: code.as_str().into(),
            message: code.message(1),
            count: NonZeroU64::MIN,
            line: Some(line_number),
        });
    }
    Ok(())
}

fn invalid(line: usize, field: &'static str) -> ClaudeParseError {
    ClaudeParseError::InvalidField { line, field }
}

#[derive(Debug)]
pub enum ClaudeParseError {
    InvalidSourcePath,
    MissingMetadata { field: &'static str },
    InvalidField { line: usize, field: &'static str },
    MalformedLine { line: usize },
    InvalidUtf8 { line: usize },
    Io { line: usize, source: io::Error },
}

impl fmt::Display for ClaudeParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSourcePath => formatter.write_str("unsupported Claude transcript path"),
            Self::MissingMetadata { field } => {
                write!(formatter, "Claude session is missing {field}")
            }
            Self::InvalidField { line, field } => {
                write!(formatter, "invalid {field} on Claude session line {line}")
            }
            Self::MalformedLine { line } => {
                write!(formatter, "malformed Claude session line {line}")
            }
            Self::InvalidUtf8 { line } => {
                write!(formatter, "invalid UTF-8 on Claude session line {line}")
            }
            Self::Io { line, .. } => write!(formatter, "could not read Claude session line {line}"),
        }
    }
}

impl Error for ClaudeParseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<JsonlError> for ClaudeParseError {
    fn from(error: JsonlError) -> Self {
        match error {
            JsonlError::MalformedLine { line } => Self::MalformedLine { line },
            JsonlError::InvalidUtf8 { line } => Self::InvalidUtf8 { line },
            JsonlError::Io { line, source } => Self::Io { line, source },
        }
    }
}
