use super::{CodexParseError, ResponseUsageWire, TokenInfoWire, TokenUsageWire};

pub(super) struct MirrorState {
    baseline: TokenUsageWire,
    confirmed: TokenUsageWire,
    thread_id: String,
    thread_total: TokenUsageWire,
    turn_id: String,
    turn_total: TokenUsageWire,
    pending: Option<PendingResponse>,
}

struct PendingResponse {
    id: String,
    usage: TokenUsageWire,
}

impl MirrorState {
    pub(super) fn new(baseline: TokenUsageWire, thread_id: String) -> Self {
        Self {
            baseline,
            confirmed: baseline,
            thread_id,
            thread_total: TokenUsageWire::default(),
            turn_id: String::new(),
            turn_total: TokenUsageWire::default(),
            pending: None,
        }
    }

    pub(super) fn require_confirmed(&self, line: usize) -> Result<(), CodexParseError> {
        if self.pending.is_some() {
            return Err(CodexParseError::InvalidField {
                line,
                field: "token_usage_record.payload.pending_mirror",
            });
        }
        Ok(())
    }

    pub(super) fn accept_response(
        &mut self,
        response: &ResponseUsageWire,
        line: usize,
    ) -> Result<(), CodexParseError> {
        self.require_confirmed(line)?;
        if response.thread_id != self.thread_id {
            return Err(CodexParseError::InvalidField {
                line,
                field: "token_usage_record.payload.thread_id",
            });
        }
        let turn_total = if self.turn_id == response.turn_id {
            self.turn_total
        } else {
            TokenUsageWire::default()
        };
        for (previous, actual, field) in [
            (
                turn_total,
                response.turn_token_usage.0,
                "token_usage_record.payload.turn_token_usage",
            ),
            (
                self.thread_total,
                response.thread_token_usage.0,
                "token_usage_record.payload.thread_token_usage",
            ),
        ] {
            if previous
                .checked_add(&response.usage.0)
                .is_none_or(|expected| !expected.same_counters(&actual))
            {
                return Err(CodexParseError::InvalidField { line, field });
            }
        }
        self.baseline
            .checked_add(&response.thread_token_usage.0)
            .ok_or(CodexParseError::InvalidField {
                line,
                field: "token_usage_record.payload.thread_token_usage",
            })?;

        self.turn_id = response.turn_id.clone();
        self.turn_total = response.turn_token_usage.0;
        self.thread_total = response.thread_token_usage.0;
        self.pending = Some(PendingResponse {
            id: response.response_id.clone(),
            usage: response.usage.0,
        });
        Ok(())
    }

    pub(super) fn validate_repeat(
        &self,
        response: &ResponseUsageWire,
        line: usize,
    ) -> Result<(), CodexParseError> {
        // Corrections after confirmation do not rewrite the historical mirror trace.
        if let Some(pending) = &self.pending
            && pending.id == response.response_id
            && (!pending.usage.same_counters(&response.usage.0)
                || !self.turn_total.same_counters(&response.turn_token_usage.0)
                || !self
                    .thread_total
                    .same_counters(&response.thread_token_usage.0))
        {
            return Err(CodexParseError::InvalidField {
                line,
                field: "token_usage_record.payload.pending_mirror",
            });
        }
        Ok(())
    }

    pub(super) fn accept_mirror(
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
        let delta = total.checked_sub(&self.confirmed).ok_or_else(invalid)?;
        if delta.is_zero()
            && !self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.usage.is_zero() && last.is_zero())
        {
            return Ok(());
        }

        let pending = self.pending.as_ref().ok_or_else(invalid)?;
        let expected = self
            .baseline
            .checked_add(&self.thread_total)
            .ok_or_else(invalid)?;
        if !total.same_counters(&expected) || !delta.same_counters(&pending.usage) {
            return Err(invalid());
        }
        if !last.same_counters(&pending.usage) {
            return Err(CodexParseError::InvalidField { line, field: LAST });
        }

        self.confirmed = total;
        self.pending = None;
        Ok(())
    }
}
