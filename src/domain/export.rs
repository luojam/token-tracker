use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{
    EstimateUnavailableReason, EstimatedCost, PricingContext, RecordedCost, ServiceTier,
    TierEvidence, TokenCounts, UsageKind,
};

pub const EXPORT_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportSnapshot {
    pub machine_id: String,
    pub machine_name: Option<String>,
    pub export_revision: u64,
    pub format_version: u32,
    pub exported_at_unix_ms: i64,
    pub events: Vec<ExportEvent>,
}

impl ExportSnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.format_version != EXPORT_FORMAT_VERSION {
            return Err("unsupported snapshot format version");
        }
        if self.export_revision == 0 {
            return Err("snapshot revision must be nonzero");
        }
        if self.machine_id.is_empty() {
            return Err("snapshot machine ID must be nonempty");
        }
        let mut identities = HashSet::new();
        for event in &self.events {
            if event.agent.is_empty() || event.event_key.is_empty() {
                return Err("event agent and key must be nonempty");
            }
            if !identities.insert((&event.agent, &event.event_key)) {
                return Err("duplicate event identity");
            }
        }
        Ok(())
    }
}

/// One deduplicated event, unique by (agent, event_key) within its machine snapshot.
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

impl Default for UsdAmount {
    fn default() -> Self {
        Self("0".into())
    }
}

impl UsdAmount {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn add(&self, other: &Self) -> Self {
        let (left_whole, left_fraction) = self.0.split_once('.').unwrap_or((&self.0, ""));
        let (right_whole, right_fraction) = other.0.split_once('.').unwrap_or((&other.0, ""));
        let scale = left_fraction.len().max(right_fraction.len());
        fn digits<'a>(
            whole: &'a str,
            fraction: &'a str,
            scale: usize,
        ) -> impl Iterator<Item = u8> + 'a {
            std::iter::repeat_n(0, scale - fraction.len())
                .chain(fraction.bytes().rev().map(|digit| digit - b'0'))
                .chain(whole.bytes().rev().map(|digit| digit - b'0'))
                .chain(std::iter::repeat(0))
        }
        let width = left_whole.len().max(right_whole.len()) + scale;
        let mut result = Vec::with_capacity(width + 1);
        let mut carry = 0;
        for (left, right) in digits(left_whole, left_fraction, scale)
            .zip(digits(right_whole, right_fraction, scale))
            .take(width)
        {
            let sum = left + right + carry;
            result.push(b'0' + sum % 10);
            carry = sum / 10;
        }
        if carry != 0 {
            result.push(b'0' + carry);
        }
        result.reverse();
        if scale != 0 {
            result.insert(result.len() - scale, b'.');
            while result.last() == Some(&b'0') {
                result.pop();
            }
            if result.last() == Some(&b'.') {
                result.pop();
            }
        }
        Self(String::from_utf8(result).expect("decimal addition produces ASCII"))
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

#[cfg(test)]
mod tests {
    use super::UsdAmount;

    #[test]
    fn decimal_addition_preserves_precision_and_normalizes_totals() {
        for (left, right, expected) in [
            ("0.1", "0.02", "0.12"),
            ("0.999", "0.0010", "1"),
            ("0.000", "0", "0"),
            (
                "340282366920938463463374607.431768211455",
                "0.00000000000000000001",
                "340282366920938463463374607.43176821145500000001",
            ),
        ] {
            let left = UsdAmount::try_from(left.to_owned()).unwrap();
            let right = UsdAmount::try_from(right.to_owned()).unwrap();
            assert_eq!(left.add(&right).as_str(), expected);
            assert_eq!(right.add(&left).as_str(), expected);
        }
    }
}
