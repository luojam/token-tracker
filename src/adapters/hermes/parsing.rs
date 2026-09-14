use std::{collections::HashSet, num::NonZeroU64, path::PathBuf};

use super::{HERMES_AGENT_ID, HermesReadError, reading::AccountingRow};
use crate::application::{
    ObservationRetention, ParseNotice, SessionData, SessionSnapshot, SnapshotCompletion,
    SourceRevision,
};
use crate::domain::{
    AgentId, ModelAttribution, ParentSession, SessionMetadata, TokenCounts, UsageEvent,
    UsageEventIdentity, UsageKind,
};

pub(super) fn normalize(
    session: &AccountingRow,
    usage: &[AccountingRow],
) -> Result<SessionSnapshot, HermesReadError> {
    let id = session.text("id")?;
    let started_at = session
        .timestamp("started_at")?
        .ok_or(HermesReadError::InvalidField("started_at"))?;
    let session_tokens = tokens(session)?;
    let metadata = SessionMetadata {
        agent: AgentId::from(HERMES_AGENT_ID),
        session_id: id.to_owned(),
        started_at,
        working_directory: session
            .optional_text("cwd")
            .filter(|cwd| !cwd.contains('\0'))
            .map(PathBuf::from)
            .filter(|cwd| cwd.is_absolute()),
        name: session.optional_text("title").map(str::to_owned),
        parent_session: session
            .optional_text("parent_session_id")
            .map(|id| ParentSession::SessionId(id.to_owned())),
    };
    let mut events = Vec::new();
    let mut identities = HashSet::new();
    let mut main_tokens = TokenCounts::default();
    for row in usage {
        let model = row.text("model")?;
        let provider = row.text("billing_provider")?;
        let endpoint = row.text("billing_base_url")?;
        let mode = row.text("billing_mode")?;
        let task = row.text("task")?;
        let identity = identity("usage", &[id, model, provider, endpoint, mode, task]);
        if !identities.insert(identity.clone()) {
            return Err(HermesReadError::InconsistentAccounting(
                "duplicate model/task identity",
            ));
        }
        let tokens = tokens(row)?;
        if task.is_empty() {
            main_tokens =
                main_tokens
                    .checked_add(tokens)
                    .ok_or(HermesReadError::InconsistentAccounting(
                        "main-loop counter sum overflow",
                    ))?;
        }
        let timestamp = row.timestamp("first_seen")?.unwrap_or(started_at);
        row.timestamp("last_seen")?;
        events.push(UsageEvent {
            identity,
            timestamp,
            kind: match task {
                "" => UsageKind::Assistant,
                "compression" => UsageKind::Compaction,
                _ => UsageKind::Other,
            },
            attribution: Some(ModelAttribution {
                provider: provider.to_owned(),
                model: model.to_owned(),
            }),
            tokens,
            recorded_cost: None,
            pricing_context: None,
        });
    }

    let residual = TokenCounts {
        input: session_tokens.input.saturating_sub(main_tokens.input),
        output: session_tokens.output.saturating_sub(main_tokens.output),
        cache_read: session_tokens
            .cache_read
            .saturating_sub(main_tokens.cache_read),
        cache_write: session_tokens
            .cache_write
            .saturating_sub(main_tokens.cache_write),
    };
    let mut notices = Vec::new();
    if residual.total() > 0 {
        events.push(UsageEvent {
            identity: identity("residual", &[id]),
            timestamp: started_at,
            kind: UsageKind::Assistant,
            attribution: None,
            tokens: residual,
            recorded_cost: None,
            pricing_context: None,
        });
        notices.push(notice("hermes_unattributed_residual", "Session counters contain main-loop usage not attributed to a model; residual cost is unavailable."));
    }
    if main_tokens.input > session_tokens.input
        || main_tokens.output > session_tokens.output
        || main_tokens.cache_read > session_tokens.cache_read
        || main_tokens.cache_write > session_tokens.cache_write
    {
        notices.push(notice(
            "hermes_counter_mismatch",
            "Main-loop model usage exceeds session counters; model usage was retained.",
        ));
    }

    Ok(SessionSnapshot {
        // Sorted rows include only selected columns; absent optional columns remain distinguishable.
        revision: SourceRevision(
            serde_json::to_vec(&("hermes-snapshot-v1", session, usage))
                .expect("accounting fields are serializable"),
        ),
        session: SessionData {
            observation_retention: ObservationRetention::ReplaceSessionObservations,
            metadata,
            events,
            completion: SnapshotCompletion::Complete,
            notices,
        },
    })
}

fn tokens(row: &AccountingRow) -> Result<TokenCounts, HermesReadError> {
    row.counter("reasoning_tokens")?;
    row.counter("api_call_count")?;
    Ok(TokenCounts {
        input: row.counter("input_tokens")?,
        output: row.counter("output_tokens")?,
        cache_read: row.counter("cache_read_tokens")?,
        cache_write: row.counter("cache_write_tokens")?,
    })
}

fn identity(kind: &str, fields: &[&str]) -> UsageEventIdentity {
    use std::fmt::Write;

    let bytes = serde_json::to_vec(fields).expect("string identity");
    let mut adapter_key = format!("hermes-{kind}-v1:");
    // Opaque encoding keeps raw endpoint credentials and queries out of diagnostic text.
    for byte in bytes {
        write!(adapter_key, "{byte:02x}").expect("writing to a string");
    }
    UsageEventIdentity {
        agent: AgentId::from(HERMES_AGENT_ID),
        adapter_key,
    }
}

fn notice(code: &str, message: &str) -> ParseNotice {
    ParseNotice {
        code: code.to_owned(),
        message: message.to_owned(),
        count: NonZeroU64::MIN,
        line: None,
    }
}
