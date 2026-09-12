use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
};

use super::{ParseNotice, SessionImport};
use crate::domain::AgentId;

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedSessionImport(SessionImport);

impl SessionImport {
    pub fn validate(
        self,
        expected_agent: &AgentId,
    ) -> Result<ValidatedSessionImport, InvalidImport> {
        if self.session.metadata.agent != *expected_agent
            || self
                .session
                .events
                .iter()
                .any(|event| event.identity.agent != *expected_agent)
        {
            return Err(InvalidImport::AgentMismatch);
        }
        let mut events = HashMap::new();
        for event in &self.session.events {
            if events
                .insert(&event.identity, event)
                .is_some_and(|previous| previous != event)
            {
                return Err(InvalidImport::ConflictingEventIdentity);
            }
            if [
                event.tokens.input,
                event.tokens.output,
                event.tokens.cache_read,
                event.tokens.cache_write,
            ]
            .into_iter()
            .any(|count| i64::try_from(count).is_err())
            {
                return Err(InvalidImport::TokenCountOutOfRange);
            }
            if event
                .pricing_context
                .as_ref()
                .is_some_and(|context| !context.usage_matches(event.tokens))
            {
                return Err(InvalidImport::BillingUsageMismatch);
            }
        }
        validate_notices(&self.session.notices)?;
        Ok(ValidatedSessionImport(self))
    }
}

impl ValidatedSessionImport {
    pub fn as_import(&self) -> &SessionImport {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidImport {
    AgentMismatch,
    ConflictingEventIdentity,
    TokenCountOutOfRange,
    BillingUsageMismatch,
    DuplicateNoticeCodes,
}

impl fmt::Display for InvalidImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AgentMismatch => "source returned usage for a different agent",
            Self::ConflictingEventIdentity => "conflicting usage events with the same identity",
            Self::TokenCountOutOfRange => "token count exceeds the supported integer range",
            Self::BillingUsageMismatch => "invalid billing usage components or totals",
            Self::DuplicateNoticeCodes => "duplicate parse notice codes",
        })
    }
}

impl Error for InvalidImport {}

pub(crate) fn validate_notices(notices: &[ParseNotice]) -> Result<(), InvalidImport> {
    let mut codes = HashSet::new();
    if notices.iter().all(|notice| codes.insert(&notice.code)) {
        Ok(())
    } else {
        Err(InvalidImport::DuplicateNoticeCodes)
    }
}
