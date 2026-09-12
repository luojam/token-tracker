use super::{
    AgentId, EstimatedCost, ModelAttribution, RecordedCost, ServiceTier, TokenCounts, UsageKind,
};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SummaryTotals {
    pub tokens: TokenCounts,
    pub recorded_cost: Option<RecordedCost>,
    pub estimates: EstimateTotals,
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
    pub estimates: EstimateTotals,
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

/// Available totals cover priced events only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EstimateTotal {
    #[default]
    Unavailable,
    Available(EstimatedCost),
    /// Overflow invalidates the entire monetary total.
    Overflow,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EstimateTotals {
    pub cost: EstimateTotal,
    /// Canonical events eligible for estimation in this total or breakdown row.
    pub imported_event_count: u64,
    pub priced_event_count: u64,
    /// Evidence counts include priced events only.
    pub requested_setting_event_count: u64,
    pub served_response_event_count: u64,
    pub assumed_standard_event_count: u64,
    /// Priced events where missing cache writes may change the cost.
    pub assumed_cache_write_event_count: u64,
    /// Each unpriced event contributes to exactly one reason.
    pub unavailable_reasons: BTreeMap<EstimateUnavailableReason, u64>,
    /// Normalized tiers for priced events.
    pub tier_event_counts: BTreeMap<ServiceTier, u64>,
    /// Price snapshot IDs and their dates (YYYY-MM-DD).
    pub rate_snapshots: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSummary {
    pub totals: SummaryTotals,
    /// Ordered by agent, then group.
    pub breakdown: Vec<SummaryBreakdown>,
}
