use std::collections::BTreeMap;

use super::SummaryError;
use crate::domain::{
    AgentId, EstimateTotal, EstimateTotals, EstimatedCost, RecordedCost, SummaryBreakdown,
    SummaryGroup, SummaryTotals, TierEvidence, TokenCounts, UsageEvent, UsageSummary,
};
use crate::pricing::EventEstimate;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CostAmount {
    Estimated(EstimatedCost),
    /// Includes recorded costs, which use floating-point USD.
    Usd(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CostTotal {
    Absent,
    Unavailable,
    Overflow,
    Available { amount: CostAmount, partial: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportTotals {
    pub tokens: TokenCounts,
    pub cost: CostTotal,
    pub estimates: EstimateTotals,
    pub session_count: u64,
    pub unique_usage_event_count: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportRow {
    pub agent: AgentId,
    pub group: SummaryGroup,
    pub tokens: TokenCounts,
    pub cost: CostTotal,
    pub estimates: EstimateTotals,
    pub unique_usage_event_count: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageReport {
    pub totals: ReportTotals,
    pub rows: Vec<ReportRow>,
}

pub fn build_usage_report(summary: &UsageSummary) -> UsageReport {
    let totals = ReportTotals {
        tokens: summary.totals.tokens,
        cost: aggregate_cost(summary.totals.recorded_cost, &summary.totals.estimates),
        estimates: summary.totals.estimates.clone(),
        session_count: summary.totals.session_count,
        unique_usage_event_count: summary.totals.unique_usage_event_count,
    };
    let rows = summary
        .breakdown
        .iter()
        .map(|row| ReportRow {
            agent: row.agent.clone(),
            group: row.group.clone(),
            tokens: row.tokens,
            cost: aggregate_cost(row.recorded_cost, &row.estimates),
            estimates: row.estimates.clone(),
            unique_usage_event_count: row.unique_usage_event_count,
        })
        .collect();
    UsageReport { totals, rows }
}

fn aggregate_cost(recorded: Option<RecordedCost>, estimates: &EstimateTotals) -> CostTotal {
    let estimated = match estimates.cost {
        EstimateTotal::Available(cost) => Some(cost),
        EstimateTotal::Unavailable => None,
        EstimateTotal::Overflow => return CostTotal::Overflow,
    };
    let amount = match (recorded, estimated) {
        (Some(recorded), estimated) => {
            let cost = recorded.as_usd()
                + estimated.map_or(0.0, |cost| cost.as_picodollars() as f64 / 1e12);
            if !cost.is_finite() {
                return CostTotal::Overflow;
            }
            CostAmount::Usd(cost)
        }
        (None, Some(estimated)) => CostAmount::Estimated(estimated),
        (None, None) if estimates.imported_event_count > 0 => return CostTotal::Unavailable,
        (None, None) => return CostTotal::Absent,
    };
    CostTotal::Available {
        amount,
        partial: estimates.priced_event_count < estimates.imported_event_count,
    }
}

pub(super) fn summarize_canonical_usage<'a>(
    session_count: u64,
    events: impl IntoIterator<Item = &'a UsageEvent>,
) -> Result<UsageSummary, SummaryError> {
    let mut totals = SummaryTotals {
        session_count,
        ..SummaryTotals::default()
    };
    let mut breakdown = BTreeMap::<(AgentId, SummaryGroup), SummaryBreakdown>::new();
    for event in events {
        totals.unique_usage_event_count = totals
            .unique_usage_event_count
            .checked_add(1)
            .ok_or(SummaryError::Overflow("event count"))?;
        totals.tokens = add_tokens(totals.tokens, event.tokens)?;
        add_cost(&mut totals.recorded_cost, event.recorded_cost)?;
        let group = match &event.attribution {
            Some(attribution) => SummaryGroup::ProviderModel(attribution.clone()),
            None => SummaryGroup::Unattributed(event.kind),
        };
        let row = breakdown
            .entry((event.identity.agent.clone(), group.clone()))
            .or_insert(SummaryBreakdown {
                agent: event.identity.agent.clone(),
                group: group.clone(),
                tokens: TokenCounts::default(),
                recorded_cost: None,
                estimates: EstimateTotals::default(),
                unique_usage_event_count: 0,
            });
        row.tokens = add_tokens(row.tokens, event.tokens)?;
        add_cost(&mut row.recorded_cost, event.recorded_cost)?;
        row.unique_usage_event_count = row
            .unique_usage_event_count
            .checked_add(1)
            .ok_or(SummaryError::Overflow("event count"))?;
        if let Some(estimate) = crate::pricing::calculate_estimate(event) {
            add_estimate(&mut totals.estimates, &estimate);
            add_estimate(&mut row.estimates, &estimate);
        }
    }
    Ok(UsageSummary {
        totals,
        breakdown: breakdown.into_values().collect(),
    })
}

fn add_estimate(totals: &mut EstimateTotals, estimate: &EventEstimate) {
    if let Some((snapshot, date)) = estimate.rate_snapshot {
        totals.rate_snapshots.insert(snapshot.into(), date.into());
    }
    totals.imported_event_count += 1;
    match estimate.result {
        Ok(value) => {
            *totals
                .tier_event_counts
                .entry(estimate.tier.clone())
                .or_default() += 1;
            totals.priced_event_count += 1;
            totals.assumed_cache_write_event_count +=
                u64::from(value.assumed_cache_writes_as_input);
            match estimate.evidence {
                TierEvidence::RequestedSetting => totals.requested_setting_event_count += 1,
                TierEvidence::ServedResponse => totals.served_response_event_count += 1,
                TierEvidence::Unknown => totals.assumed_standard_event_count += 1,
            }
            totals.cost = match totals.cost {
                EstimateTotal::Unavailable => EstimateTotal::Available(value.cost),
                EstimateTotal::Available(current) => current
                    .checked_add(value.cost)
                    .map(EstimateTotal::Available)
                    .unwrap_or(EstimateTotal::Overflow),
                EstimateTotal::Overflow => EstimateTotal::Overflow,
            };
        }
        Err(reason) => *totals.unavailable_reasons.entry(reason).or_default() += 1,
    }
}

fn add_tokens(current: TokenCounts, value: TokenCounts) -> Result<TokenCounts, SummaryError> {
    current
        .checked_add(value)
        .ok_or(SummaryError::Overflow("token total"))
}

fn add_cost(
    current: &mut Option<RecordedCost>,
    value: Option<RecordedCost>,
) -> Result<(), SummaryError> {
    if let Some(value) = value {
        *current = Some(match *current {
            Some(current) => current
                .checked_add(value)
                .map_err(|_| SummaryError::Overflow("recorded cost"))?,
            None => value,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{EstimateUnavailableReason, ServiceTier, UsageEstimate};

    #[test]
    fn estimate_total_overflow_is_sticky_without_losing_coverage() {
        let mut totals = EstimateTotals::default();
        let mut estimate = EventEstimate {
            result: Ok(UsageEstimate::default()),
            tier: ServiceTier::Standard,
            evidence: TierEvidence::RequestedSetting,
            rate_snapshot: None,
        };
        for cost in [u128::MAX, 1, 0] {
            estimate.result = Ok(UsageEstimate {
                cost: EstimatedCost::from_picodollars(cost),
                ..UsageEstimate::default()
            });
            add_estimate(&mut totals, &estimate);
        }
        estimate.result = Err(EstimateUnavailableReason::ArithmeticOverflow);
        add_estimate(&mut totals, &estimate);
        assert_eq!(totals.cost, EstimateTotal::Overflow);
        assert_eq!(totals.imported_event_count, 4);
        assert_eq!(totals.priced_event_count, 3);
        assert_eq!(totals.requested_setting_event_count, 3);
        assert_eq!(
            totals.unavailable_reasons[&EstimateUnavailableReason::ArithmeticOverflow],
            1
        );
        assert_eq!(
            aggregate_cost(Some(RecordedCost::from_usd(1.0).unwrap()), &totals),
            CostTotal::Overflow
        );
    }
}
