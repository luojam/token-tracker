pub mod anthropic;
pub mod openai;

use crate::domain::{
    AgentId, EstimateBreakdown, EstimateSummary, EstimateTotal, EstimateTotals,
    EstimateUnavailableReason, ServiceTier, SummaryGroup, TierEvidence, UsageEstimate, UsageEvent,
};
use std::collections::BTreeMap;

/// Includes events with pricing context or no recorded cost, even if unpriced.
pub fn summarize_estimates<'a>(
    events: impl IntoIterator<Item = &'a UsageEvent>,
) -> BTreeMap<AgentId, EstimateSummary> {
    let mut estimates = BTreeMap::<AgentId, EstimateSummary>::new();
    let mut rows = BTreeMap::<(AgentId, SummaryGroup, ServiceTier), EstimateTotals>::new();
    for event in events {
        let context = event.pricing_context.as_ref();
        if context.is_none() && event.recorded_cost.is_some() {
            continue;
        }
        let provider = context
            .map(|context| context.provider.as_str())
            .or_else(|| {
                event
                    .attribution
                    .as_ref()
                    .map(|attribution| attribution.provider.as_str())
            });
        let (estimate, tier, evidence, snapshot) = match provider {
            Some("openai") => {
                let (tier, evidence) = context
                    .map(openai::estimate_tier)
                    .unwrap_or((ServiceTier::Unknown, TierEvidence::Unknown));
                (
                    openai::calculate_estimate(
                        event,
                        openai::MissingCacheWritePolicy::TreatAsInput,
                    ),
                    tier,
                    evidence,
                    Some((openai::SNAPSHOT_ID, openai::RATE_DATE)),
                )
            }
            Some("anthropic") => (
                anthropic::calculate_estimate(event),
                context
                    .map(|context| context.tier.clone())
                    .unwrap_or(ServiceTier::Unknown),
                context
                    .map(|context| context.tier_evidence)
                    .unwrap_or(TierEvidence::Unknown),
                Some((anthropic::SNAPSHOT_ID, anthropic::RATE_DATE)),
            ),
            Some(_) => (
                Err(EstimateUnavailableReason::UnsupportedProvider),
                context
                    .map(|context| context.tier.clone())
                    .unwrap_or(ServiceTier::Unknown),
                context
                    .map(|context| context.tier_evidence)
                    .unwrap_or(TierEvidence::Unknown),
                None,
            ),
            None => (
                Err(EstimateUnavailableReason::MissingPricingContext),
                ServiceTier::Unknown,
                TierEvidence::Unknown,
                None,
            ),
        };
        let summary = estimates
            .entry(event.identity.agent.clone())
            .or_insert_with(|| EstimateSummary {
                rate_snapshots: BTreeMap::new(),
                totals: EstimateTotals::default(),
                breakdown: Vec::new(),
            });
        if let Some((snapshot, date)) = snapshot {
            summary.rate_snapshots.insert(snapshot.into(), date.into());
        }
        let group = event
            .attribution
            .as_ref()
            .map(|attribution| SummaryGroup::ProviderModel(attribution.clone()))
            .unwrap_or(SummaryGroup::Unattributed(event.kind));
        let row = rows
            .entry((event.identity.agent.clone(), group, tier))
            .or_default();
        add_estimate(&mut summary.totals, estimate, evidence);
        add_estimate(row, estimate, evidence);
    }
    for ((agent, group, tier), totals) in rows {
        estimates
            .get_mut(&agent)
            .expect("estimate row has a summary")
            .breakdown
            .push(EstimateBreakdown {
                group,
                tier,
                totals,
            });
    }
    estimates
}

fn add_estimate(
    totals: &mut EstimateTotals,
    estimate: Result<UsageEstimate, EstimateUnavailableReason>,
    evidence: TierEvidence,
) {
    totals.imported_event_count += 1;
    match estimate {
        Ok(estimate) => {
            totals.priced_event_count += 1;
            totals.assumed_cache_write_event_count +=
                u64::from(estimate.assumed_cache_writes_as_input);
            match evidence {
                TierEvidence::RequestedSetting => totals.requested_setting_event_count += 1,
                TierEvidence::ServedResponse => totals.served_response_event_count += 1,
                TierEvidence::Unknown => totals.assumed_standard_event_count += 1,
            }
            totals.cost = match totals.cost {
                EstimateTotal::Unavailable => EstimateTotal::Available(estimate.cost),
                EstimateTotal::Available(current) => current
                    .checked_add(estimate.cost)
                    .map(EstimateTotal::Available)
                    .unwrap_or(EstimateTotal::Overflow),
                EstimateTotal::Overflow => EstimateTotal::Overflow,
            };
        }
        Err(reason) => *totals.unavailable_reasons.entry(reason).or_default() += 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::EstimatedCost;

    #[test]
    fn estimate_total_overflow_is_sticky_without_losing_coverage() {
        let mut totals = EstimateTotals::default();
        for cost in [u128::MAX, 1, 0] {
            add_estimate(
                &mut totals,
                Ok(UsageEstimate {
                    cost: EstimatedCost::from_picodollars(cost),
                    ..UsageEstimate::default()
                }),
                TierEvidence::RequestedSetting,
            );
        }
        add_estimate(
            &mut totals,
            Err(EstimateUnavailableReason::ArithmeticOverflow),
            TierEvidence::RequestedSetting,
        );
        assert_eq!(totals.cost, EstimateTotal::Overflow);
        assert_eq!(totals.imported_event_count, 4);
        assert_eq!(totals.priced_event_count, 3);
        assert_eq!(totals.requested_setting_event_count, 3);
        assert_eq!(
            totals.unavailable_reasons[&EstimateUnavailableReason::ArithmeticOverflow],
            1
        );
    }
}
