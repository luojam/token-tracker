pub mod anthropic;
pub mod openai;

use crate::domain::{
    EstimateUnavailableReason, PricingContext, ServiceTier, TierEvidence, UsageEstimate, UsageEvent,
};

pub struct EventEstimate {
    pub result: Result<UsageEstimate, EstimateUnavailableReason>,
    pub tier: ServiceTier,
    pub evidence: TierEvidence,
    pub rate_snapshot: Option<(&'static str, &'static str)>,
}

/// Recorded costs take precedence over estimates.
pub fn calculate_estimate(event: &UsageEvent) -> Option<EventEstimate> {
    if event.recorded_cost.is_some() {
        return None;
    }
    let (estimate, tier, evidence, snapshot) = match event.pricing_context.as_ref() {
        Some(PricingContext::OpenAi(context)) => {
            let (tier, evidence) = openai::estimate_tier(context);
            (
                openai::calculate_estimate(event, openai::MissingCacheWritePolicy::TreatAsInput),
                tier,
                evidence,
                Some((openai::SNAPSHOT_ID, openai::RATE_DATE)),
            )
        }
        Some(PricingContext::Anthropic(context)) => (
            anthropic::calculate_estimate(event),
            context.tier.clone(),
            context.tier_evidence,
            Some((anthropic::SNAPSHOT_ID, anthropic::RATE_DATE)),
        ),
        None => {
            let provider = event
                .attribution
                .as_ref()
                .map(|model| model.provider.as_str());
            let snapshot = match provider {
                Some("openai") => Some((openai::SNAPSHOT_ID, openai::RATE_DATE)),
                Some("anthropic") => Some((anthropic::SNAPSHOT_ID, anthropic::RATE_DATE)),
                _ => None,
            };
            let reason = match provider {
                Some("openai" | "anthropic") | None => {
                    EstimateUnavailableReason::MissingPricingContext
                }
                Some(_) => EstimateUnavailableReason::UnsupportedProvider,
            };
            (
                Err(reason),
                ServiceTier::Unknown,
                TierEvidence::Unknown,
                snapshot,
            )
        }
    };
    Some(EventEstimate {
        result: estimate,
        tier,
        evidence,
        rate_snapshot: snapshot,
    })
}
