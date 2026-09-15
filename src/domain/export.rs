use serde::{Deserialize, Serialize};

use super::{
    EstimateUnavailableReason, EstimatedCost, PricingContext, RecordedCost, ServiceTier,
    TierEvidence, TokenCounts, UsageKind,
};

pub const EXPORT_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSnapshot {
    pub machine_id: String,
    pub export_revision: u64,
    pub format_version: u32,
    pub exported_at_unix_ms: i64,
    pub events: Vec<ExportEvent>,
}

/// One reconciled event, unique by (agent, event_key) within its machine snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportEvent {
    pub agent: String,
    pub event_key: String,
    pub timestamp_unix_ms: i64,
    pub usage_kind: UsageKind,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub tokens: TokenCounts,
    pub recorded_cost_usd: Option<UsdAmount>,
    pub estimate: ExportEstimate,
    pub pricing_context: Option<PricingContext>,
    /// Memberships can overlap; expanding this array must not multiply usage.
    pub sessions: Vec<ExportSession>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExportEstimate {
    Available {
        cost_usd: UsdAmount,
        pricing_version: String,
        pricing_date: String,
        tier: ServiceTier,
        tier_evidence: TierEvidence,
        assumptions: EstimateAssumptions,
    },
    Unavailable {
        reason: EstimateUnavailableReason,
        pricing_version: Option<String>,
        pricing_date: Option<String>,
        tier: ServiceTier,
        tier_evidence: TierEvidence,
    },
    /// A recorded cost exists, so estimation was skipped.
    NotNeeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EstimateAssumptions {
    pub standard_rates: bool,
    pub cache_writes_as_input: bool,
    pub short_context: bool,
}

/// Session and parent IDs are scoped to the event's agent and snapshot's machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSession {
    pub session_id: String,
    pub started_at_unix_ms: i64,
    pub name: Option<String>,
    pub working_directory: Option<String>,
    pub parent_session: Option<ExportParentSession>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportParentSession {
    SessionId(String),
    SourcePath(String),
}

/// Nonnegative decimal USD string, without an exponent or display rounding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct UsdAmount(String);

impl UsdAmount {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for UsdAmount {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let (whole, fraction) = value
            .split_once('.')
            .map_or((value.as_str(), None), |(whole, fraction)| {
                (whole, Some(fraction))
            });
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !digits(whole)
            || (whole.len() > 1 && whole.starts_with('0'))
            || fraction.is_some_and(|part| !digits(part))
        {
            return Err("USD amount must be a nonnegative decimal string without an exponent");
        }
        Ok(Self(value))
    }
}

impl From<RecordedCost> for UsdAmount {
    fn from(cost: RecordedCost) -> Self {
        let value = cost.as_usd();
        Self(if value == 0.0 {
            "0".into()
        } else {
            value.to_string()
        })
    }
}

impl From<EstimatedCost> for UsdAmount {
    fn from(cost: EstimatedCost) -> Self {
        const PICODOLLARS_PER_DOLLAR: u128 = 1_000_000_000_000;
        let value = cost.as_picodollars();
        let whole = value / PICODOLLARS_PER_DOLLAR;
        let fraction = value % PICODOLLARS_PER_DOLLAR;
        if fraction == 0 {
            Self(whole.to_string())
        } else {
            Self(
                format!("{whole}.{fraction:012}")
                    .trim_end_matches('0')
                    .into(),
            )
        }
    }
}
