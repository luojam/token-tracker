use super::CodexParseError;
use crate::domain::Timestamp;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct TurnLifecycle {
    active: Option<ActiveTurn>,
    started_turns: BTreeSet<String>,
}

enum ActiveTurn {
    Accounting(AccountingTurn),
    Review(ReviewTurn),
}

pub(super) struct AccountingTurn {
    pub(super) id: String,
    pub(super) started_at: Timestamp,
    has_context: bool,
}

struct ReviewTurn {
    id: String,
    thread_id: Option<String>,
    phase: ReviewPhase,
}

enum ReviewPhase {
    Forwarding { child_started: bool },
    AwaitingCompletion,
}

pub(super) enum ReviewBoundary {
    Enter,
    Exit,
}

impl TurnLifecycle {
    /// Returns whether the boundary should update pricing context.
    pub(super) fn accept_boundary(
        &mut self,
        boundary: &str,
        turn_id: String,
        timestamp: Timestamp,
        line: usize,
    ) -> Result<bool, CodexParseError> {
        let invalid = || CodexParseError::InvalidField {
            line,
            field: "event_msg.payload.turn_id",
        };
        if boundary == "task_started" {
            let forwarded = match &self.active {
                None => false,
                Some(ActiveTurn::Review(ReviewTurn {
                    phase:
                        ReviewPhase::Forwarding {
                            child_started: false,
                        },
                    ..
                })) => true,
                _ => return Err(invalid()),
            };
            if !self.started_turns.insert(turn_id.clone()) {
                return Err(invalid());
            }
            if let Some(ActiveTurn::Review(review)) = &mut self.active {
                review.phase = ReviewPhase::Forwarding {
                    child_started: true,
                };
            } else {
                self.active = Some(ActiveTurn::Accounting(AccountingTurn {
                    id: turn_id,
                    started_at: timestamp,
                    has_context: false,
                }));
            }
            return Ok(!forwarded);
        }

        let accounting = match &self.active {
            Some(ActiveTurn::Accounting(turn)) if turn.id == turn_id => true,
            Some(ActiveTurn::Review(review))
                if review.id == turn_id
                    && matches!(review.phase, ReviewPhase::AwaitingCompletion) =>
            {
                false
            }
            _ => return Err(invalid()),
        };
        self.active = None;
        Ok(accounting)
    }

    pub(super) fn accept_review(
        &mut self,
        boundary: ReviewBoundary,
        turn_id: String,
        thread_id: Option<String>,
        line: usize,
    ) -> Result<(), CodexParseError> {
        let invalid = || CodexParseError::InvalidField {
            line,
            field: "event_msg.payload.turn_id",
        };
        match boundary {
            ReviewBoundary::Enter => {
                if self.active.is_some() || !self.started_turns.insert(turn_id.clone()) {
                    return Err(invalid());
                }
                // Review entry starts the parent turn; forwarded child starts do not.
                self.active = Some(ActiveTurn::Review(ReviewTurn {
                    id: turn_id,
                    thread_id,
                    phase: ReviewPhase::Forwarding {
                        child_started: false,
                    },
                }));
            }
            ReviewBoundary::Exit => {
                let Some(ActiveTurn::Review(review)) = &mut self.active else {
                    return Err(invalid());
                };
                if review.id != turn_id || !matches!(review.phase, ReviewPhase::Forwarding { .. }) {
                    return Err(invalid());
                }
                if review.thread_id != thread_id {
                    return Err(CodexParseError::InvalidField {
                        line,
                        field: "event_msg.payload.thread_id",
                    });
                }
                // Exit precedes the parent's completion or abort, not the child's.
                review.phase = ReviewPhase::AwaitingCompletion;
            }
        }
        Ok(())
    }

    pub(super) fn in_review(&self) -> bool {
        matches!(self.active, Some(ActiveTurn::Review(_)))
    }

    pub(super) fn accept_context(
        &mut self,
        turn_id: &str,
        line: usize,
    ) -> Result<(), CodexParseError> {
        match &mut self.active {
            Some(ActiveTurn::Accounting(turn)) if turn.id == turn_id => {
                turn.has_context = true;
                Ok(())
            }
            _ => Err(CodexParseError::InvalidField {
                line,
                field: "turn_context.payload.turn_id",
            }),
        }
    }

    pub(super) fn accounting_turn(&self) -> Option<&AccountingTurn> {
        match &self.active {
            Some(ActiveTurn::Accounting(turn)) if turn.has_context => Some(turn),
            _ => None,
        }
    }
}
