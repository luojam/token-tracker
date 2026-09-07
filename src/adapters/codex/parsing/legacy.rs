use super::{CODEX_AGENT_ID, CodexParseError, TokenInfoWire, TokenUsageWire};
use crate::core::{AgentId, Timestamp, UsageEvent, UsageEventIdentity, UsageKind};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct LegacyUsageState {
    pub(super) events: BTreeMap<String, UsageEvent>,
    active_turn: Option<ActiveTurn>,
    started_turns: BTreeSet<String>,
    previous_total: Option<TokenUsageWire>,
    checkpoint_before_usage: bool,
}

struct ActiveTurn {
    id: String,
    started_at: Timestamp,
    has_context: bool,
}

impl LegacyUsageState {
    pub(super) fn accept_boundary(
        &mut self,
        boundary: &str,
        turn_id: String,
        timestamp: Timestamp,
        line: usize,
    ) -> Result<(), CodexParseError> {
        let invalid = || CodexParseError::InvalidField {
            line,
            field: "event_msg.payload.turn_id",
        };
        if boundary == "task_started" {
            if self.active_turn.is_some() || !self.started_turns.insert(turn_id.clone()) {
                return Err(invalid());
            }
            self.active_turn = Some(ActiveTurn {
                id: turn_id,
                started_at: timestamp,
                has_context: false,
            });
        } else {
            if self
                .active_turn
                .as_ref()
                .is_none_or(|turn| turn.id != turn_id)
            {
                return Err(invalid());
            }
            self.active_turn = None;
        }
        Ok(())
    }

    pub(super) fn accept_context(
        &mut self,
        turn_id: &str,
        line: usize,
    ) -> Result<(), CodexParseError> {
        let turn = self
            .active_turn
            .as_mut()
            .filter(|turn| turn.id == turn_id)
            .ok_or(CodexParseError::InvalidField {
                line,
                field: "turn_context.payload.turn_id",
            })?;
        turn.has_context = true;
        Ok(())
    }

    pub(super) fn accept_compaction(&mut self) {
        if self.previous_total.is_none() {
            self.checkpoint_before_usage = true;
        }
    }

    pub(super) fn baseline(&self) -> TokenUsageWire {
        self.previous_total.unwrap_or_default()
    }

    pub(super) fn validate_response_start(
        &self,
        turn_id: &str,
        line: usize,
    ) -> Result<(), CodexParseError> {
        if self.checkpoint_before_usage
            || self.events.contains_key(turn_id)
            || self
                .active_turn
                .as_ref()
                .is_none_or(|turn| turn.id != turn_id || !turn.has_context)
        {
            return Err(CodexParseError::InvalidField {
                line,
                field: "token_usage_record.payload.turn_id",
            });
        }
        Ok(())
    }

    pub(super) fn accept_usage(
        &mut self,
        info: TokenInfoWire,
        line: usize,
    ) -> Result<(), CodexParseError> {
        const TOTAL: &str = "event_msg.payload.info.total_token_usage";
        const LAST: &str = "event_msg.payload.info.last_token_usage";
        let invalid = || CodexParseError::InvalidField { line, field: TOTAL };
        let total = info.total_token_usage.0;
        let last = info.last_token_usage.0;
        total.normalize(line, TOTAL)?;
        let delta = total
            .checked_sub(&self.previous_total.unwrap_or_default())
            .ok_or_else(invalid)?;
        let tokens = delta.normalize(line, TOTAL)?;
        let unchanged = self.previous_total.is_some() && delta.is_zero();

        // Recomputed last usage can describe context size, not an incurred request.
        if unchanged && !last.is_zero() {
            return Ok(());
        }
        if self.checkpoint_before_usage {
            return Err(invalid());
        }
        let turn = match self.active_turn.as_ref().filter(|turn| turn.has_context) {
            Some(turn) => turn,
            None if unchanged => return Ok(()),
            None => return Err(invalid()),
        };
        if !delta.same_counters(&last) {
            return Err(CodexParseError::InvalidField { line, field: LAST });
        }

        let event = self
            .events
            .entry(turn.id.clone())
            .or_insert_with(|| UsageEvent {
                identity: UsageEventIdentity {
                    agent: AgentId::from(CODEX_AGENT_ID),
                    adapter_key: format!("legacy-turn-v1:{}", turn.id),
                },
                timestamp: turn.started_at,
                kind: UsageKind::Other,
                attribution: None,
                tokens: Default::default(),
                recorded_cost: None,
                pricing_context: None,
            });
        event.tokens = event.tokens.checked_add(tokens).ok_or_else(invalid)?;
        self.previous_total = Some(total);
        Ok(())
    }
}

impl TokenUsageWire {
    pub(super) fn checked_sub(&self, previous: &Self) -> Option<Self> {
        let cache_write = self
            .cache_write_input_tokens
            .unwrap_or(0)
            .checked_sub(previous.cache_write_input_tokens.unwrap_or(0))?;
        Some(Self {
            input_tokens: self.input_tokens.checked_sub(previous.input_tokens)?,
            cached_input_tokens: self
                .cached_input_tokens
                .checked_sub(previous.cached_input_tokens)?,
            cache_write_input_tokens: self.cache_write_input_tokens.map(|_| cache_write),
            output_tokens: self.output_tokens.checked_sub(previous.output_tokens)?,
            reasoning_output_tokens: self
                .reasoning_output_tokens
                .checked_sub(previous.reasoning_output_tokens)?,
            total_tokens: self.total_tokens.checked_sub(previous.total_tokens)?,
        })
    }

    pub(super) fn same_counters(&self, other: &Self) -> bool {
        self.input_tokens == other.input_tokens
            && self.cached_input_tokens == other.cached_input_tokens
            && self.cache_write_input_tokens.unwrap_or(0)
                == other.cache_write_input_tokens.unwrap_or(0)
            && self.output_tokens == other.output_tokens
            && self.reasoning_output_tokens == other.reasoning_output_tokens
            && self.total_tokens == other.total_tokens
    }

    pub(super) fn is_zero(&self) -> bool {
        self.same_counters(&Self::default())
    }
}
