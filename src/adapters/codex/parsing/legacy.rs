use super::{
    CODEX_AGENT_ID, CodexParseError, TokenInfoWire, TokenUsageWire,
    context::{ContextState, RawServiceTier},
    lifecycle::AccountingTurn,
};
use crate::domain::{
    AgentId, CacheDetail, KnownRequests, PricingContext, RequestBreakdown, ServiceTier,
    TierEvidence, UsageEvent, UsageEventIdentity, UsageKind,
};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct LegacyUsageState {
    pub(super) events: BTreeMap<String, UsageEvent>,
    raw_tiers: BTreeMap<String, RawServiceTier>,
    previous_total: Option<TokenUsageWire>,
    checkpoint_before_usage: bool,
}

impl LegacyUsageState {
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
        if self.checkpoint_before_usage || self.events.contains_key(turn_id) {
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
        context: &ContextState,
        turn: Option<&AccountingTurn>,
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
        let turn = match turn {
            Some(turn) => turn,
            None if unchanged => return Ok(()),
            None => return Err(invalid()),
        };
        if !delta.same_counters(&last) {
            return Err(CodexParseError::InvalidField { line, field: LAST });
        }

        if unchanged && self.events.contains_key(&turn.id) {
            return Ok(());
        }
        let cache_detail = if total.cache_detail() == CacheDetail::Complete
            && self
                .previous_total
                .is_none_or(|previous| previous.cache_detail() == CacheDetail::Complete)
        {
            CacheDetail::Complete
        } else {
            CacheDetail::Incomplete
        };
        let (attribution, pricing_context, raw_tier) = context.observation(
            &turn.id,
            None,
            RequestBreakdown::AggregateOrUnknown,
            cache_detail,
        );
        let original_tier = self
            .raw_tiers
            .entry(turn.id.clone())
            .or_insert_with(|| raw_tier.clone());
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
                attribution: attribution.clone(),
                tokens: Default::default(),
                recorded_cost: None,
                pricing_context: Some(PricingContext::OpenAi(pricing_context.clone())),
            });
        if event.attribution != attribution {
            event.attribution = None;
        }
        if let Some(PricingContext::OpenAi(existing)) = &mut event.pricing_context {
            match &mut existing.requests {
                RequestBreakdown::KnownRequests(requests) => requests.push(tokens),
                _ => {
                    existing.requests = RequestBreakdown::KnownRequests(KnownRequests::new(tokens))
                }
            }
            if existing.tier != pricing_context.tier
                || *original_tier != raw_tier
                || existing.tier_evidence != pricing_context.tier_evidence
            {
                existing.tier = ServiceTier::Unknown;
                existing.tier_evidence = TierEvidence::Unknown;
            }
            if cache_detail == CacheDetail::Incomplete {
                existing.cache_detail = CacheDetail::Incomplete;
            }
        }
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
