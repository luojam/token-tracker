use super::SqliteStoreError;
use crate::core::{
    CacheDetail, PricingContext, RawServiceTier, RequestGranularity, ServiceTier, TierEvidence,
};

pub(super) fn encode(context: Option<&PricingContext>) -> [Option<&str>; 7] {
    let Some(context) = context else {
        return [None; 7];
    };
    let (tier, unsupported) = match &context.tier {
        ServiceTier::Standard => ("standard", None),
        ServiceTier::Fast => ("fast", None),
        ServiceTier::Unknown => ("unknown", None),
        ServiceTier::Unsupported(value) => ("unsupported", Some(value.as_str())),
    };
    let (raw_kind, raw_value) = match &context.raw_tier {
        RawServiceTier::Missing => ("missing", None),
        RawServiceTier::Null => ("null", None),
        RawServiceTier::Value(value) => ("value", Some(value.as_str())),
    };
    [
        Some(tier),
        unsupported,
        Some(raw_kind),
        raw_value,
        Some(match context.tier_evidence {
            TierEvidence::Unknown => "unknown",
            TierEvidence::RequestedSetting => "requested_setting",
            TierEvidence::ServedResponse => "served_response",
        }),
        Some(match context.request_granularity {
            RequestGranularity::ExactSingleRequest => "exact_single_request",
            RequestGranularity::AggregateOrUnknown => "aggregate_or_unknown",
        }),
        Some(match context.cache_detail {
            CacheDetail::Complete => "complete",
            CacheDetail::Incomplete => "incomplete",
        }),
    ]
}

pub(super) fn decode(
    columns: [Option<String>; 7],
) -> Result<Option<PricingContext>, SqliteStoreError> {
    if columns.iter().all(Option::is_none) {
        return Ok(None);
    }
    let invalid = || SqliteStoreError::CorruptData("an invalid pricing context");
    let [
        tier,
        unsupported,
        raw_kind,
        raw_value,
        evidence,
        granularity,
        cache,
    ] = columns.each_ref().map(|value| value.as_deref());
    Ok(Some(PricingContext {
        tier: match (tier, unsupported) {
            (Some("standard"), None) => ServiceTier::Standard,
            (Some("fast"), None) => ServiceTier::Fast,
            (Some("unknown"), None) => ServiceTier::Unknown,
            (Some("unsupported"), Some(value)) => ServiceTier::Unsupported(value.to_owned()),
            _ => return Err(invalid()),
        },
        raw_tier: match (raw_kind, raw_value) {
            (Some("missing"), None) => RawServiceTier::Missing,
            (Some("null"), None) => RawServiceTier::Null,
            (Some("value"), Some(value)) => RawServiceTier::Value(value.to_owned()),
            _ => return Err(invalid()),
        },
        tier_evidence: match evidence {
            Some("unknown") => TierEvidence::Unknown,
            Some("requested_setting") => TierEvidence::RequestedSetting,
            Some("served_response") => TierEvidence::ServedResponse,
            _ => return Err(invalid()),
        },
        request_granularity: match granularity {
            Some("exact_single_request") => RequestGranularity::ExactSingleRequest,
            Some("aggregate_or_unknown") => RequestGranularity::AggregateOrUnknown,
            _ => return Err(invalid()),
        },
        cache_detail: match cache {
            Some("complete") => CacheDetail::Complete,
            Some("incomplete") => CacheDetail::Incomplete,
            _ => return Err(invalid()),
        },
    }))
}
