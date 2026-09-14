use super::reading::AccountingRow;
use crate::domain::{
    AnthropicBilling, CacheDetail, OpenAiBilling, PricingContext, RecordedCost, RequestBreakdown,
    ServiceSpeed, ServiceTier, TierEvidence,
};

pub(super) fn provider<'a>(raw: &'a str, endpoint: &str) -> &'a str {
    match raw {
        "openai-codex" | "openai-api" => "openai",
        "" | "auto" => endpoint_provider(endpoint).unwrap_or(raw),
        _ => raw,
    }
}

fn endpoint_provider(endpoint: &str) -> Option<&'static str> {
    let url = endpoint.strip_prefix("https://")?;
    let authority = url.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.strip_suffix(":443").unwrap_or(host);
    match host {
        "api.openai.com" => Some("openai"),
        "api.anthropic.com" => Some("anthropic"),
        "chatgpt.com" if url[authority.len()..].starts_with("/backend-api/codex") => Some("openai"),
        _ => None,
    }
}

pub(super) fn pricing_context(
    provider: &str,
    endpoint: &str,
    api_call_count: u64,
) -> Option<PricingContext> {
    if !endpoint.is_empty() && endpoint_provider(endpoint) != Some(provider) {
        return None;
    }

    match provider {
        "openai" => Some(PricingContext::OpenAi(OpenAiBilling {
            tier: ServiceTier::Unknown,
            tier_evidence: TierEvidence::Unknown,
            requests: if api_call_count == 1 {
                RequestBreakdown::SingleRequest
            } else {
                RequestBreakdown::AggregateAssumingShortContext
            },
            // A cumulative zero cannot prove cache-write reporting was complete.
            cache_detail: CacheDetail::Incomplete,
        })),
        "anthropic" => Some(PricingContext::Anthropic(AnthropicBilling {
            tier: ServiceTier::Unknown,
            tier_evidence: TierEvidence::Unknown,
            speed: ServiceSpeed::Unknown,
            requests: RequestBreakdown::AggregateOrUnknown,
            cache_writes: None,
        })),
        _ => None,
    }
}

pub(super) fn subscription(row: &AccountingRow) -> bool {
    row.optional_text("cost_status") == Some("included")
        || matches!(
            row.optional_text("billing_mode"),
            Some("subscription" | "subscription_included")
        )
}

pub(super) fn recorded_cost(row: &AccountingRow) -> Result<Option<RecordedCost>, ()> {
    if row.optional_text("cost_status") != Some("actual") || subscription(row) {
        return Ok(None);
    }
    // Status/source describe the latest call, so they cannot prove multi-call cost coverage.
    if row.counter("api_call_count").ok() != Some(1)
        || !matches!(
            row.optional_text("cost_source"),
            Some("provider_cost_api" | "provider_generation_api")
        )
        || matches!(
            row.optional_text("billing_mode"),
            Some("mixed" | "partial" | "incomplete")
        )
    {
        return Err(());
    }
    row.number("actual_cost_usd")
        .ok_or(())
        .and_then(|amount| RecordedCost::from_usd(amount).map_err(|_| ()))
        .map(Some)
}
