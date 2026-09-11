mod billing;
mod summary;
pub use billing::{CacheWriteTokens, PricingContext};
pub use summary::*;

use std::error::Error;
use std::fmt;
use std::path::PathBuf;

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

/// Disjoint counts: input excludes cache reads/writes; output includes reasoning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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

/// API token estimate in picodollars (10^-12 USD).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct EstimatedCost(u128);

impl EstimatedCost {
    pub const fn from_picodollars(value: u128) -> Self {
        Self(value)
    }

    pub const fn as_picodollars(self) -> u128 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }
}

impl fmt::Display for EstimatedCost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const PICODOLLARS_PER_MICRODOLLAR: u128 = 1_000_000;
        const MICRODOLLARS_PER_DOLLAR: u128 = 1_000_000;

        if self.0 > 0 && self.0 < PICODOLLARS_PER_MICRODOLLAR {
            return formatter.write_str("<$0.000001");
        }
        // Divide before rounding so even u128::MAX can be displayed.
        let microdollars = self.0 / PICODOLLARS_PER_MICRODOLLAR
            + u128::from(self.0 % PICODOLLARS_PER_MICRODOLLAR >= 500_000);
        write!(
            formatter,
            "${}.{:06}",
            microdollars / MICRODOLLARS_PER_DOLLAR,
            microdollars % MICRODOLLARS_PER_DOLLAR,
        )
    }
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTier {
    Standard,
    Fast,
    Unknown,
    Unsupported(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TierEvidence {
    Unknown,
    RequestedSetting,
    ServedResponse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestGranularity {
    ExactSingleRequest,
    AggregateOrUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDetail {
    Complete,
    /// Cache writes may be included in ordinary input instead of reported separately.
    Incomplete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UsageKind {
    Assistant,
    ToolResult,
    Compaction,
    BranchSummary,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelAttribution {
    pub provider: String,
    pub model: String,
}

/// Stable across forks and clones; independent of paths, usage counts, and scan times.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageEventIdentity {
    pub agent: AgentId,
    pub adapter_key: String,
}

/// Additive usage: cumulative counters must be converted to increments.
/// Equal identities represent observations of the same usage.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageEvent {
    pub identity: UsageEventIdentity,
    pub timestamp: Timestamp,
    pub kind: UsageKind,
    pub attribution: Option<ModelAttribution>,
    pub tokens: TokenCounts,
    pub recorded_cost: Option<RecordedCost>,
    pub pricing_context: Option<PricingContext>,
}

/// Parent references are scoped to the child's agent. Paths must be absolute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentSession {
    SessionId(String),
    SourcePath(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionMetadata {
    pub agent: AgentId,
    pub session_id: String,
    pub working_directory: Option<PathBuf>,
    pub started_at: Timestamp,
    pub name: Option<String>,
    pub parent_session: Option<ParentSession>,
}
