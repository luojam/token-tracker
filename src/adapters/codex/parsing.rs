use std::num::NonZeroU32;

use super::CODEX_AGENT_ID;
use crate::adapters::jsonl::{JsonlError, JsonlLine, JsonlReader};
use crate::application::{ParseCompletion, ParseContext, ParsedSession, SessionParser};
use crate::domain::{
    AgentId, CacheDetail, ParentSession, PricingContext, RequestBreakdown, SessionMetadata,
    Timestamp, TokenCounts, UsageEvent, UsageEventIdentity, UsageKind,
};
use chrono::DateTime;
use serde::de::{DeserializeOwned, MapAccess, Visitor, value::MapAccessDeserializer};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::io::{self, BufRead};
use std::marker::PhantomData;
use std::path::PathBuf;
use std::{error::Error, fmt};

mod context;
mod legacy;
mod lifecycle;
mod mirrors;

use context::{ContextState, SettingsWire};
use legacy::LegacyUsageState;
use lifecycle::{ReviewBoundary, TurnLifecycle};
use mirrors::MirrorState;

#[derive(Clone, Copy, Debug, Default)]
pub struct CodexSessionParser;

impl CodexSessionParser {
    pub const fn new() -> Self {
        Self
    }
}

impl SessionParser for CodexSessionParser {
    type Error = CodexParseError;

    fn normalization_version(&self) -> NonZeroU32 {
        super::NORMALIZATION_VERSION
    }

    fn parse(
        &self,
        input: &mut dyn BufRead,
        _context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error> {
        let mut lines = JsonlReader::new(input);
        let mut session: Option<SessionState> = None;
        let mut completion = ParseCompletion::Complete;

        loop {
            let (line_number, text) = match lines.next_line()? {
                JsonlLine::Complete { number, text } => (number, text),
                JsonlLine::Incomplete { .. } => {
                    if session.is_none() {
                        return Err(CodexParseError::IncompleteHeader);
                    }
                    completion = ParseCompletion::IncompleteFinalLine;
                    break;
                }
                JsonlLine::Eof => break,
            };
            let entry: TypeWire = decode(text, line_number, "type")?;
            if entry.entry_type == "session_meta" {
                validate_timestamp(text, line_number)?;
                let header: HeaderWire = payload(text, line_number, "session_meta.payload")?;
                let header = header.normalize(line_number)?;
                match session.as_mut() {
                    Some(session) => session.accept_header(header, line_number)?,
                    None => session = Some(SessionState::new(header)),
                }
            } else {
                let session = session.as_mut().ok_or(CodexParseError::InvalidHeader)?;
                session.inherited_prefix = false;
                if entry.entry_type == "token_usage_record" {
                    let timestamp = validate_timestamp(text, line_number)?;
                    let response = payload(text, line_number, "token_usage_record.payload")?;
                    session.accept_response(response, timestamp, line_number)?;
                } else {
                    session.accept_entry(text, &entry.entry_type, line_number)?;
                }
            }
        }

        let session = session.ok_or(CodexParseError::MissingHeader)?;
        Ok(ParsedSession {
            metadata: session.metadata,
            events: session
                .legacy
                .events
                .into_values()
                .chain(
                    session
                        .responses
                        .into_values()
                        .map(|response| response.event),
                )
                .collect(),
            completion,
            notices: Vec::new(),
        })
    }
}

fn decode<T: DeserializeOwned>(
    text: &str,
    line: usize,
    field: &'static str,
) -> Result<T, CodexParseError> {
    serde_json::from_str::<ObjectWire<T>>(text)
        .map(|object| object.0)
        .map_err(|_| CodexParseError::InvalidField { line, field })
}

fn payload<T: DeserializeOwned>(
    text: &str,
    line: usize,
    field: &'static str,
) -> Result<T, CodexParseError> {
    decode::<PayloadWire<T>>(text, line, field).map(|entry| entry.payload.0)
}

fn parse_timestamp(
    value: &str,
    line: usize,
    field: &'static str,
) -> Result<Timestamp, CodexParseError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| Timestamp::from_unix_milliseconds(timestamp.timestamp_millis()))
        .map_err(|_| CodexParseError::InvalidField { line, field })
}

fn validate_timestamp(text: &str, line: usize) -> Result<Timestamp, CodexParseError> {
    let entry: TimestampWire = decode(text, line, "timestamp")?;
    parse_timestamp(&entry.timestamp, line, "timestamp")
}

struct SessionState {
    metadata: SessionMetadata,
    headers: BTreeMap<String, HeaderIdentity>,
    expected_ancestor: Option<String>,
    inherited_prefix: bool,
    responses: BTreeMap<String, ResponseObservation>,
    legacy: LegacyUsageState,
    turns: TurnLifecycle,
    mirrors: Option<MirrorState>,
    context: ContextState,
}

impl SessionState {
    fn new(header: NormalizedHeader) -> Self {
        let mut context = ContextState::default();
        context.accept_header(
            &header.metadata.session_id,
            header.provider,
            header.identity.forked_from_id.is_none(),
        );
        Self {
            expected_ancestor: header.identity.forked_from_id.clone(),
            headers: BTreeMap::from([(header.metadata.session_id.clone(), header.identity)]),
            metadata: header.metadata,
            inherited_prefix: true,
            responses: BTreeMap::new(),
            legacy: LegacyUsageState::default(),
            turns: TurnLifecycle::default(),
            mirrors: None,
            context,
        }
    }

    fn accept_entry(
        &mut self,
        text: &str,
        entry_type: &str,
        line: usize,
    ) -> Result<(), CodexParseError> {
        match entry_type {
            "turn_context" => {
                validate_timestamp(text, line)?;
                let turn: TurnContextWire = payload(text, line, "turn_context.payload")?;
                self.turns.accept_context(&turn.turn_id, line)?;
                self.context.accept_context(turn.model);
            }
            "event_msg" => {
                let event: TypeWire = payload(text, line, "event_msg.payload.type")?;
                match event.entry_type.as_str() {
                    "token_count" => {
                        validate_timestamp(text, line)?;
                        let count: TokenCountWire = payload(text, line, "event_msg.payload.info")?;
                        if let Some(info) = count.info {
                            if self.expected_ancestor.is_some() {
                                return Err(CodexParseError::InvalidField {
                                    line,
                                    field: "event_msg.payload.info.total_token_usage",
                                });
                            }
                            match &mut self.mirrors {
                                Some(mirrors) => mirrors.accept_mirror(info.0, line)?,
                                None => self.legacy.accept_usage(
                                    info.0,
                                    &self.context,
                                    self.turns.accounting_turn(),
                                    line,
                                )?,
                            }
                        }
                    }
                    "thread_settings_applied" => {
                        validate_timestamp(text, line)?;
                        let settings: SettingsWire =
                            payload(text, line, "event_msg.payload.thread_settings")?;
                        self.context
                            .accept_settings(settings, !self.turns.in_review());
                    }
                    "task_started" | "task_complete" | "turn_aborted" => {
                        let timestamp = validate_timestamp(text, line)?;
                        let turn: TurnWire = payload(text, line, "event_msg.payload.turn_id")?;
                        if let Some(mirrors) = &self.mirrors {
                            mirrors.require_confirmed(line)?;
                        }
                        if self.turns.accept_boundary(
                            &event.entry_type,
                            turn.turn_id.clone(),
                            timestamp,
                            line,
                        )? {
                            self.context
                                .accept_boundary(&event.entry_type, &turn.turn_id);
                        }
                    }
                    "entered_review_mode" | "exited_review_mode" => {
                        validate_timestamp(text, line)?;
                        let turn: TurnWire = payload(text, line, "event_msg.payload.turn_id")?;
                        let boundary = if event.entry_type == "entered_review_mode" {
                            ReviewBoundary::Enter
                        } else {
                            ReviewBoundary::Exit
                        };
                        self.accept_review(boundary, turn.turn_id, None, line)?;
                    }
                    "item_completed" => {
                        let item: ItemCompletedWire =
                            payload(text, line, "event_msg.payload.item")?;
                        let boundary = match item.item.0.entry_type.as_str() {
                            "EnteredReviewMode" => Some(ReviewBoundary::Enter),
                            "ExitedReviewMode" => Some(ReviewBoundary::Exit),
                            _ => None,
                        };
                        if let Some(boundary) = boundary {
                            validate_timestamp(text, line)?;
                            let turn: ReviewItemWire =
                                payload(text, line, "event_msg.payload.review")?;
                            self.accept_review(boundary, turn.turn_id, Some(turn.thread_id), line)?;
                        }
                    }
                    _ => {}
                }
            }
            "compacted" => {
                validate_timestamp(text, line)?;
                if self.mirrors.is_none() {
                    self.legacy.accept_compaction();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn accept_review(
        &mut self,
        boundary: ReviewBoundary,
        turn_id: String,
        thread_id: Option<String>,
        line: usize,
    ) -> Result<(), CodexParseError> {
        if thread_id
            .as_ref()
            .is_some_and(|id| !self.headers.contains_key(id))
        {
            return Err(CodexParseError::InvalidField {
                line,
                field: "event_msg.payload.thread_id",
            });
        }
        if let Some(mirrors) = &self.mirrors {
            mirrors.require_confirmed(line)?;
        }
        self.turns.accept_review(boundary, turn_id, thread_id, line)
    }

    fn accept_response(
        &mut self,
        response: ResponseUsageWire,
        timestamp: Timestamp,
        line: usize,
    ) -> Result<(), CodexParseError> {
        if !self.headers.contains_key(&response.thread_id) {
            return Err(CodexParseError::InvalidField {
                line,
                field: "token_usage_record.payload.thread_id",
            });
        }
        let tokens = response
            .usage
            .0
            .normalize(line, "token_usage_record.payload.usage")?;
        response
            .turn_token_usage
            .0
            .normalize(line, "token_usage_record.payload.turn_token_usage")?;
        response
            .thread_token_usage
            .0
            .normalize(line, "token_usage_record.payload.thread_token_usage")?;
        response.turn_token_usage.0.validate_contains(
            &response.usage.0,
            line,
            "token_usage_record.payload.turn_token_usage",
        )?;
        response.thread_token_usage.0.validate_contains(
            &response.turn_token_usage.0,
            line,
            "token_usage_record.payload.thread_token_usage",
        )?;

        if let Some(original) = self.responses.get_mut(&response.response_id) {
            for (changed, field) in [
                (
                    original.thread_id != response.thread_id,
                    "token_usage_record.payload.thread_id",
                ),
                (
                    original.turn_id != response.turn_id,
                    "token_usage_record.payload.turn_id",
                ),
                (
                    original.session_id != response.session_id,
                    "token_usage_record.payload.session_id",
                ),
                (
                    original.root_turn_id != response.root_turn_id,
                    "token_usage_record.payload.root_turn_id",
                ),
            ] {
                if changed {
                    return Err(CodexParseError::InvalidField { line, field });
                }
            }
            if let Some(mirrors) = &self.mirrors {
                mirrors.accept_repeat(&response, line)?;
            }
            // Corrections retain the original request identity and timestamp.
            original.event.tokens = tokens;
            if let Some(PricingContext::OpenAi(context)) = &mut original.event.pricing_context {
                context.cache_detail = response.usage.0.cache_detail();
            }
        } else {
            if self
                .turns
                .accounting_turn()
                .is_none_or(|turn| turn.id != response.turn_id)
            {
                return Err(CodexParseError::InvalidField {
                    line,
                    field: "token_usage_record.payload.turn_id",
                });
            }
            self.legacy
                .validate_response_start(&response.turn_id, line)?;
            if self.expected_ancestor.is_some() || response.thread_id != self.metadata.session_id {
                return Err(CodexParseError::InvalidField {
                    line,
                    field: "token_usage_record.payload.thread_id",
                });
            }
            self.mirrors
                .get_or_insert_with(|| {
                    MirrorState::new(self.legacy.baseline(), response.thread_id.clone())
                })
                .accept_response(&response, line)?;
            let (attribution, pricing_context, _) = self.context.observation(
                &response.turn_id,
                Some(&response.thread_id),
                RequestBreakdown::SingleRequest,
                response.usage.0.cache_detail(),
            );
            let event = UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::from(CODEX_AGENT_ID),
                    adapter_key: format!("response-v1:{}", response.response_id),
                },
                timestamp,
                kind: UsageKind::Other,
                attribution,
                tokens,
                recorded_cost: None,
                pricing_context: Some(PricingContext::OpenAi(pricing_context)),
            };
            self.responses.insert(
                response.response_id,
                ResponseObservation {
                    thread_id: response.thread_id,
                    turn_id: response.turn_id,
                    session_id: response.session_id,
                    root_turn_id: response.root_turn_id,
                    event,
                },
            );
        }
        Ok(())
    }

    fn accept_header(
        &mut self,
        header: NormalizedHeader,
        line: usize,
    ) -> Result<(), CodexParseError> {
        let id = &header.metadata.session_id;
        if let Some(original) = self.headers.get(id) {
            if header.identity.started_at != original.started_at {
                return Err(CodexParseError::InvalidField {
                    line,
                    field: "session_meta.payload.timestamp",
                });
            }
            if header.identity.parent_session != original.parent_session
                || header.identity.forked_from_id != original.forked_from_id
            {
                return Err(CodexParseError::InvalidField {
                    line,
                    field: "session_meta.payload.parent",
                });
            }
            self.context.accept_header(
                id,
                header.provider,
                id == &self.metadata.session_id
                    && (original.forked_from_id.is_none() || !self.inherited_prefix),
            );
            return Ok(());
        }

        if !self.inherited_prefix || self.expected_ancestor.as_ref() != Some(id) {
            return Err(CodexParseError::InvalidField {
                line,
                field: "session_meta.payload.id",
            });
        }
        if header
            .identity
            .parent_session
            .as_ref()
            .is_some_and(|parent| self.headers.contains_key(parent))
        {
            return Err(CodexParseError::InvalidField {
                line,
                field: "session_meta.payload.parent",
            });
        }
        self.expected_ancestor = header.identity.forked_from_id.clone();
        // Copied legacy turns cannot establish thread ownership.
        self.context.accept_header(id, header.provider, false);
        self.headers.insert(id.clone(), header.identity);
        Ok(())
    }
}

struct ResponseObservation {
    thread_id: String,
    turn_id: String,
    session_id: Option<String>,
    root_turn_id: Option<String>,
    event: UsageEvent,
}

struct HeaderIdentity {
    started_at: Timestamp,
    parent_session: Option<String>,
    forked_from_id: Option<String>,
}

struct NormalizedHeader {
    metadata: SessionMetadata,
    identity: HeaderIdentity,
    provider: Option<String>,
}

impl HeaderWire {
    fn normalize(self, line: usize) -> Result<NormalizedHeader, CodexParseError> {
        for (value, field) in [
            (Some(&self.id), "session_meta.payload.id"),
            (
                self.parent_thread_id.as_ref(),
                "session_meta.payload.parent_thread_id",
            ),
            (
                self.forked_from_id.as_ref(),
                "session_meta.payload.forked_from_id",
            ),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(CodexParseError::InvalidField { line, field });
            }
        }
        if matches!(
            (&self.parent_thread_id, &self.forked_from_id),
            (Some(parent), Some(fork)) if parent != fork
        ) {
            return Err(CodexParseError::InvalidField {
                line,
                field: "session_meta.payload.parent",
            });
        }
        let parent = self
            .parent_thread_id
            .or_else(|| self.forked_from_id.clone());
        if parent.as_ref() == Some(&self.id) {
            return Err(CodexParseError::InvalidField {
                line,
                field: "session_meta.payload.parent",
            });
        }
        let started_at = parse_timestamp(&self.timestamp, line, "session_meta.payload.timestamp")?;
        Ok(NormalizedHeader {
            provider: self.model_provider,
            metadata: SessionMetadata {
                agent: AgentId::from(CODEX_AGENT_ID),
                session_id: self.id,

                working_directory: self.cwd.map(PathBuf::from),
                started_at,
                name: None,
                parent_session: parent.clone().map(ParentSession::SessionId),
            },
            identity: HeaderIdentity {
                started_at,
                parent_session: parent,
                forked_from_id: self.forked_from_id,
            },
        })
    }
}

#[derive(Deserialize)]
struct TypeWire {
    #[serde(rename = "type")]
    entry_type: String,
}

#[derive(Deserialize)]
struct TimestampWire {
    timestamp: String,
}

#[derive(Deserialize)]
struct PayloadWire<T> {
    payload: ObjectWire<T>,
}

struct ObjectWire<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for ObjectWire<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
            type Value = ObjectWire<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(MapAccessDeserializer::new(map)).map(ObjectWire)
            }
        }

        deserializer.deserialize_map(ObjectVisitor(PhantomData))
    }
}

#[derive(Deserialize)]
struct HeaderWire {
    id: String,
    timestamp: String,
    cwd: Option<String>,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    model_provider: Option<String>,
}

#[derive(Deserialize)]
struct TurnContextWire {
    #[serde(deserialize_with = "nonempty_id")]
    turn_id: String,
    model: Option<String>,
}

#[derive(Deserialize)]
struct TurnWire {
    #[serde(deserialize_with = "nonempty_id")]
    turn_id: String,
}

#[derive(Deserialize)]
struct ItemCompletedWire {
    item: ObjectWire<TypeWire>,
}

#[derive(Deserialize)]
struct ReviewItemWire {
    #[serde(deserialize_with = "nonempty_id")]
    thread_id: String,
    #[serde(deserialize_with = "nonempty_id")]
    turn_id: String,
}

#[derive(Deserialize)]
struct ResponseUsageWire {
    #[serde(deserialize_with = "nonempty_id")]
    response_id: String,
    #[serde(deserialize_with = "nonempty_id")]
    thread_id: String,
    #[serde(deserialize_with = "nonempty_id")]
    turn_id: String,
    session_id: Option<String>,
    root_turn_id: Option<String>,
    usage: ObjectWire<TokenUsageWire>,
    turn_token_usage: ObjectWire<TokenUsageWire>,
    thread_token_usage: ObjectWire<TokenUsageWire>,
}

#[derive(Deserialize)]
struct TokenCountWire {
    info: Option<ObjectWire<TokenInfoWire>>,
}

#[derive(Deserialize)]
struct TokenInfoWire {
    total_token_usage: ObjectWire<TokenUsageWire>,
    last_token_usage: ObjectWire<TokenUsageWire>,
}

#[derive(Clone, Copy, Default, Deserialize)]
struct TokenUsageWire {
    input_tokens: u64,
    cached_input_tokens: u64,
    #[serde(default, deserialize_with = "present_counter")]
    cache_write_input_tokens: Option<u64>,
    output_tokens: u64,
    reasoning_output_tokens: u64,
    total_tokens: u64,
}

impl TokenUsageWire {
    fn cache_detail(&self) -> CacheDetail {
        if self.cache_write_input_tokens.is_some() {
            CacheDetail::Complete
        } else {
            CacheDetail::Incomplete
        }
    }

    fn validate_contains(
        &self,
        usage: &Self,
        line: usize,
        field: &'static str,
    ) -> Result<(), CodexParseError> {
        if self.input_tokens < usage.input_tokens
            || self.cached_input_tokens < usage.cached_input_tokens
            || self.normalize(line, field)?.input < usage.normalize(line, field)?.input
            || self.output_tokens < usage.output_tokens
            || self.reasoning_output_tokens < usage.reasoning_output_tokens
            || self.total_tokens < usage.total_tokens
            || matches!(
                (self.cache_write_input_tokens, usage.cache_write_input_tokens),
                (Some(total), Some(used)) if total < used
            )
        {
            return Err(CodexParseError::InvalidField { line, field });
        }
        Ok(())
    }

    fn normalize(&self, line: usize, field: &'static str) -> Result<TokenCounts, CodexParseError> {
        let invalid = || CodexParseError::InvalidField { line, field };
        if self.input_tokens.checked_add(self.output_tokens) != Some(self.total_tokens)
            || self.reasoning_output_tokens > self.output_tokens
        {
            return Err(invalid());
        }
        let cache_write = self.cache_write_input_tokens.unwrap_or(0);
        let cached = self
            .cached_input_tokens
            .checked_add(cache_write)
            .ok_or_else(invalid)?;
        let input = self.input_tokens.checked_sub(cached).ok_or_else(invalid)?;
        Ok(TokenCounts {
            input,
            output: self.output_tokens,
            cache_read: self.cached_input_tokens,
            cache_write,
        })
    }
}

fn nonempty_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(serde::de::Error::custom("empty identifier"));
    }
    Ok(value)
}

fn present_counter<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(deserializer).map(Some)
}

#[derive(Debug)]
pub enum CodexParseError {
    MissingHeader,
    IncompleteHeader,
    InvalidHeader,
    InvalidField { line: usize, field: &'static str },
    MalformedLine { line: usize },
    InvalidUtf8 { line: usize },
    Io { line: usize, kind: io::ErrorKind },
}

impl fmt::Display for CodexParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHeader => formatter.write_str("Codex session is missing its header"),
            Self::IncompleteHeader => formatter.write_str("Codex session header is incomplete"),
            Self::InvalidHeader => {
                formatter.write_str("first JSONL value is not a Codex session header")
            }
            Self::InvalidField { line, field } => {
                write!(formatter, "invalid {field} on Codex session line {line}")
            }
            Self::MalformedLine { line } => {
                write!(formatter, "malformed Codex session line {line}")
            }
            Self::InvalidUtf8 { line } => {
                write!(formatter, "invalid UTF-8 on Codex session line {line}")
            }
            Self::Io { line, kind } => {
                write!(
                    formatter,
                    "could not read Codex session line {line}: {kind}"
                )
            }
        }
    }
}

impl Error for CodexParseError {}

impl From<JsonlError> for CodexParseError {
    fn from(error: JsonlError) -> Self {
        match error {
            JsonlError::MalformedLine { line } => Self::MalformedLine { line },
            JsonlError::InvalidUtf8 { line } => Self::InvalidUtf8 { line },
            JsonlError::Io { line, source } => Self::Io {
                line,
                kind: source.kind(),
            },
        }
    }
}
