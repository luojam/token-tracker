mod anthropic;

pub use anthropic::{
    AnthropicIteration, AnthropicIterationKind, AnthropicPricingContext, AnthropicUsage,
    AnthropicUsageComponent, CacheCreationTokens, RawServedValue,
};

use std::collections::BTreeMap;
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

/// Disjoint token categories: input excludes cache reads/writes, and output
/// includes any reasoning tokens. Adapters normalize overlapping source counters.
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

/// API-equivalent USD value in picodollars (10^-12 USD), not a reported charge.
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServiceTier {
    Standard,
    Fast,
    Unknown,
    Unsupported(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RawServiceTier {
    Missing,
    Null,
    Value(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TierEvidence {
    Unknown,
    RequestedSetting,
    ServedResponse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestGranularity {
    ExactSingleRequest,
    AggregateOrUnknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheDetail {
    Complete,
    /// Cache writes may be included in ordinary input instead of reported separately.
    Incomplete,
}

/// Facts bound to the original request; no rates, derived counts, or money.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricingContext {
    /// Standard/fast require requested or served evidence bound to this usage.
    pub tier: ServiceTier,
    /// Retained even when attribution is unknown; missing, null, and auto differ.
    pub raw_tier: RawServiceTier,
    pub tier_evidence: TierEvidence,
    pub request_granularity: RequestGranularity,
    pub cache_detail: CacheDetail,
    /// Complete per-request breakdown, retaining the aggregate's ledger identity.
    pub request_usage: Option<Vec<TokenCounts>>,
    /// Uses its own served evidence and replaces the request_usage breakdown.
    pub anthropic: Option<AnthropicPricingContext>,
}

impl PricingContext {
    pub fn for_anthropic(facts: AnthropicPricingContext) -> Self {
        Self {
            tier: ServiceTier::Unknown,
            raw_tier: RawServiceTier::Missing,
            tier_evidence: TierEvidence::Unknown,
            request_granularity: RequestGranularity::ExactSingleRequest,
            cache_detail: CacheDetail::Complete,
            request_usage: None,
            anthropic: Some(facts),
        }
    }

    pub fn usage_matches(&self, tokens: TokenCounts) -> bool {
        self.request_usage_matches(tokens)
            && self
                .anthropic
                .as_ref()
                .is_none_or(|facts| self.request_usage.is_none() && facts.usage_matches(tokens))
    }

    pub fn request_usage_matches(&self, tokens: TokenCounts) -> bool {
        self.request_usage.as_ref().is_none_or(|requests| {
            !requests.is_empty()
                && requests
                    .iter()
                    .try_fold(TokenCounts::default(), |sum, request| {
                        sum.checked_add(*request)
                    })
                    == Some(tokens)
        })
    }
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

/// Adapter-defined event identity that remains stable across forks and clones.
/// Paths, usage values, and observation times must not affect it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsageEventIdentity {
    pub agent: AgentId,
    pub adapter_key: String,
}

/// One additive usage event, excluding conversation and tool content.
/// Adapters convert cumulative counters to increments. Equal identities
/// describe observations of the same incurred usage.
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
    /// Source format version, when reported (not the adapter implementation version).
    pub format_version: Option<String>,
    pub working_directory: Option<PathBuf>,
    pub started_at: Timestamp,
    pub name: Option<String>,
    pub parent_session: Option<ParentSession>,
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
    pub agent: AgentId,
    pub group: SummaryGroup,
    pub tokens: TokenCounts,
    pub recorded_cost: Option<RecordedCost>,
    pub unique_usage_event_count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EstimateUnavailableReason {
    MissingPricingContext,
    UnknownAttribution,
    UnsupportedProvider,
    UnsupportedModel,
    UnknownTier,
    UnsupportedTier,
    UnknownSpeed,
    UnsupportedSpeed,
    InvalidUsageBreakdown,
    UnknownRequestGranularity,
    UnsupportedContextBand,
    IncompleteCacheDetail,
    ArithmeticOverflow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageEstimate {
    pub cost: EstimatedCost,
    pub assumed_cache_writes_as_input: bool,
}

/// An available total covers only priced events; coverage determines partiality.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EstimateTotal {
    #[default]
    Unavailable,
    Available(EstimatedCost),
    /// The sum overflowed, so no partial monetary total may be displayed.
    Overflow,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EstimateTotals {
    pub cost: EstimateTotal,
    /// Imported canonical Codex events only, not completeness of local history.
    pub imported_event_count: u64,
    pub priced_event_count: u64,
    /// Evidence counts include priced events only.
    pub requested_setting_event_count: u64,
    pub served_response_event_count: u64,
    pub assumed_standard_event_count: u64,
    /// Priced events where missing cache writes may change the cost.
    pub assumed_cache_write_event_count: u64,
    /// Exactly one deterministic reason per unpriced event.
    pub unavailable_reasons: BTreeMap<EstimateUnavailableReason, u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EstimateBreakdown {
    pub attribution: Option<ModelAttribution>,
    pub tier: ServiceTier,
    pub totals: EstimateTotals,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EstimateSummary {
    pub snapshot_id: String,
    /// Price snapshot date (YYYY-MM-DD), not the usage date.
    pub rate_date: String,
    pub totals: EstimateTotals,
    /// Ordered by original attribution and estimated tier, retaining unsupported values.
    pub breakdown: Vec<EstimateBreakdown>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSummary {
    pub totals: SummaryTotals,
    /// Rows must be ordered deterministically by agent, then group.
    pub breakdown: Vec<SummaryBreakdown>,
    /// Separate from recorded costs; absent when there are no Codex observations.
    pub estimate: Option<EstimateSummary>,
}
