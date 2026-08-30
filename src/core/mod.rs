//! Agent-neutral domain types and token-usage rules.

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

/// Open agent identifier that does not require core changes for new adapters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for AgentId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for AgentId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An instant represented as Unix milliseconds in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const fn from_unix_milliseconds(value: i64) -> Self {
        Self(value)
    }

    pub const fn as_unix_milliseconds(self) -> i64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenCounts {
    pub fn total(self) -> u128 {
        u128::from(self.input)
            + u128::from(self.output)
            + u128::from(self.cache_read)
            + u128::from(self.cache_write)
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            input: self.input.checked_add(other.input)?,
            output: self.output.checked_add(other.output)?,
            cache_read: self.cache_read.checked_add(other.cache_read)?,
            cache_write: self.cache_write.checked_add(other.cache_write)?,
        })
    }
}

/// A source-reported USD cost, never derived from token counts.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct RecordedCost(f64);

impl RecordedCost {
    pub fn from_usd(value: f64) -> Result<Self, InvalidRecordedCost> {
        if !value.is_finite() {
            return Err(InvalidRecordedCost::NotFinite);
        }
        if value < 0.0 {
            return Err(InvalidRecordedCost::Negative);
        }
        Ok(Self(value))
    }

    pub const fn as_usd(self) -> f64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Result<Self, InvalidRecordedCost> {
        Self::from_usd(self.0 + other.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidRecordedCost {
    Negative,
    NotFinite,
}

impl fmt::Display for InvalidRecordedCost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Negative => formatter.write_str("recorded cost cannot be negative"),
            Self::NotFinite => formatter.write_str("recorded cost must be finite"),
        }
    }
}

impl Error for InvalidRecordedCost {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageKind {
    Assistant,
    ToolResult,
    Compaction,
    BranchSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelAttribution {
    pub provider: String,
    pub model: String,
}

/// Adapter-defined event identity that remains stable across forks and clones.
/// Paths, usage values, and observation times must not affect it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageEventIdentity {
    pub agent: AgentId,
    pub adapter_key: String,
}

/// One source's usage observation, excluding conversation and tool content.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageEvent {
    pub identity: UsageEventIdentity,
    pub timestamp: Timestamp,
    pub kind: UsageKind,
    pub attribution: Option<ModelAttribution>,
    pub tokens: TokenCounts,
    pub recorded_cost: Option<RecordedCost>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMetadata {
    pub agent: AgentId,
    pub session_id: String,
    pub format_version: u32,
    pub working_directory: PathBuf,
    pub started_at: Timestamp,
    pub name: Option<String>,
    pub parent_session: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SummaryTotals {
    pub tokens: TokenCounts,
    pub recorded_cost: Option<RecordedCost>,
    pub session_count: u64,
    pub unique_usage_event_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SummaryGroup {
    ProviderModel(ModelAttribution),
    Unattributed(UsageKind),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SummaryBreakdown {
    pub group: SummaryGroup,
    pub tokens: TokenCounts,
    pub recorded_cost: Option<RecordedCost>,
    pub unique_usage_event_count: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSummary {
    pub totals: SummaryTotals,
    /// Rows must be ordered deterministically by group.
    pub breakdown: Vec<SummaryBreakdown>,
}
