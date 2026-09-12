use std::{collections::HashSet, error::Error, fmt};

use super::{ParseNotice, SessionImport};
use crate::domain::AgentId;

#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedSessionImport(SessionImport);

impl SessionImport {
    pub fn validate(
        self,
        expected_agent: &AgentId,
    ) -> Result<ValidatedSessionImport, InvalidImport> {
        if self.parsed.metadata.agent != *expected_agent
            || self
                .parsed
                .events
                .iter()
                .any(|event| event.identity.agent != *expected_agent)
        {
            return Err(InvalidImport::AgentMismatch);
        }
        for event in &self.parsed.events {
            if event
                .pricing_context
                .as_ref()
                .is_some_and(|context| !context.usage_matches(event.tokens))
            {
                return Err(InvalidImport::BillingUsageMismatch);
            }
        }
        validate_notices(&self.parsed.notices)?;
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
    BillingUsageMismatch,
    DuplicateNoticeCodes,
}

impl fmt::Display for InvalidImport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AgentMismatch => "parser returned usage for a different agent",
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
