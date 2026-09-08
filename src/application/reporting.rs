use std::fmt::Write;

use super::ImportWarning;
use crate::core::{
    EstimateSummary, EstimateTotal, EstimateTotals, EstimateUnavailableReason, ModelAttribution,
    ServiceTier, SummaryGroup, UsageKind, UsageSummary,
};

pub fn render_terminal_report(summary: &UsageSummary, warnings: &[ImportWarning]) -> String {
    let mut output = String::new();
    let totals = &summary.totals;

    writeln!(output, "Token Tracker — All Time").unwrap();
    writeln!(output).unwrap();
    writeln!(
        output,
        "Input tokens: {}",
        format_integer(totals.tokens.input)
    )
    .unwrap();
    writeln!(
        output,
        "Output tokens: {}",
        format_integer(totals.tokens.output)
    )
    .unwrap();
    writeln!(
        output,
        "Cache-read tokens: {}",
        format_integer(totals.tokens.cache_read)
    )
    .unwrap();
    writeln!(
        output,
        "Cache-write tokens: {}",
        format_integer(totals.tokens.cache_write)
    )
    .unwrap();
    writeln!(
        output,
        "Total tokens: {}",
        format_integer(totals.tokens.total())
    )
    .unwrap();
    if let Some(cost) = totals.recorded_cost {
        writeln!(output, "Recorded cost: ${:.6}", cost.as_usd()).unwrap();
    }
    writeln!(output, "Sessions: {}", format_integer(totals.session_count)).unwrap();
    writeln!(
        output,
        "Unique usage events: {}",
        format_integer(totals.unique_usage_event_count)
    )
    .unwrap();

    writeln!(output).unwrap();
    writeln!(output, "Usage by provider/model:").unwrap();
    if summary.breakdown.is_empty() {
        writeln!(output, "- none").unwrap();
    } else {
        for row in &summary.breakdown {
            write!(
                output,
                "- {}: input {}, output {}, cache read {}, cache write {}, total {}, events {}",
                group_label(&row.group),
                format_integer(row.tokens.input),
                format_integer(row.tokens.output),
                format_integer(row.tokens.cache_read),
                format_integer(row.tokens.cache_write),
                format_integer(row.tokens.total()),
                format_integer(row.unique_usage_event_count),
            )
            .unwrap();
            if let Some(cost) = row.recorded_cost {
                write!(output, ", cost ${:.6}", cost.as_usd()).unwrap();
            }
            writeln!(output).unwrap();
        }
    }

    if let Some(estimate) = &summary.estimate {
        render_estimate(&mut output, estimate);
    }

    if !warnings.is_empty() {
        let mut warnings = warnings.iter().collect::<Vec<_>>();
        warnings.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.message.cmp(&right.message))
        });

        writeln!(output).unwrap();
        writeln!(output, "Warnings ({}):", warnings.len()).unwrap();
        for warning in warnings {
            match &warning.path {
                Some(path) => writeln!(
                    output,
                    "- {}: {}",
                    one_line(&path.display().to_string()),
                    one_line(&warning.message)
                )
                .unwrap(),
                None => writeln!(output, "- {}", one_line(&warning.message)).unwrap(),
            }
        }
    }

    output
}

fn render_estimate(output: &mut String, estimate: &EstimateSummary) {
    let totals = &estimate.totals;
    writeln!(output).unwrap();
    writeln!(output, "API-equivalent estimate (Codex):").unwrap();
    writeln!(
        output,
        "Rate date: {} (snapshot: {})",
        one_line(&estimate.rate_date),
        one_line(&estimate.snapshot_id),
    )
    .unwrap();
    writeln!(output, "Estimated total: {}", estimate_cost_label(totals)).unwrap();
    writeln!(
        output,
        "Coverage: {} / {} imported canonical Codex events priced",
        format_integer(totals.priced_event_count),
        format_integer(totals.imported_event_count),
    )
    .unwrap();
    writeln!(
        output,
        "Priced tier evidence: {} requested setting, {} served response",
        format_integer(totals.requested_setting_event_count),
        format_integer(totals.served_response_event_count),
    )
    .unwrap();
    if totals.requested_setting_event_count > 0 {
        writeln!(output, "Requested settings do not confirm the served tier.").unwrap();
    }
    writeln!(output, "Estimates by provider/model/tier:").unwrap();
    for row in &estimate.breakdown {
        writeln!(
            output,
            "- {} / {}: {}, coverage {} / {}, requested setting {}, served response {}",
            row.attribution
                .as_ref()
                .map(model_label)
                .unwrap_or_else(|| "Unattributed".into()),
            tier_label(&row.tier),
            estimate_cost_label(&row.totals),
            format_integer(row.totals.priced_event_count),
            format_integer(row.totals.imported_event_count),
            format_integer(row.totals.requested_setting_event_count),
            format_integer(row.totals.served_response_event_count),
        )
        .unwrap();
    }
    if !totals.unavailable_reasons.is_empty() {
        writeln!(output, "Unpriced events:").unwrap();
        for (reason, count) in &totals.unavailable_reasons {
            writeln!(
                output,
                "- {}: {}",
                unavailable_reason_label(*reason),
                format_integer(*count),
            )
            .unwrap();
        }
    }
    writeln!(
        output,
        "Coverage excludes Pi; unparsed files have unknown usage outside this denominator."
    )
    .unwrap();
    writeln!(
        output,
        "Public API token prices at this snapshot, not historical rates or actual subscription charges. \
         Excludes regional uplifts, discounts, tool fees, and subscription/credit charges."
    )
    .unwrap();
}

fn estimate_cost_label(totals: &EstimateTotals) -> String {
    match totals.cost {
        EstimateTotal::Available(cost)
            if totals.priced_event_count < totals.imported_event_count =>
        {
            format!("{cost} (partial)")
        }
        EstimateTotal::Available(cost) => cost.to_string(),
        EstimateTotal::Unavailable => "unavailable".into(),
        EstimateTotal::Overflow => "unavailable (arithmetic overflow)".into(),
    }
}

fn tier_label(tier: &ServiceTier) -> String {
    match tier {
        ServiceTier::Standard => "standard".into(),
        ServiceTier::Fast => "fast".into(),
        ServiceTier::Unknown => "unknown".into(),
        ServiceTier::Unsupported(raw) => format!("unsupported ({})", one_line(raw)),
    }
}

fn unavailable_reason_label(reason: EstimateUnavailableReason) -> &'static str {
    match reason {
        EstimateUnavailableReason::MissingPricingContext => "missing pricing context",
        EstimateUnavailableReason::UnknownAttribution => "unknown provider/model attribution",
        EstimateUnavailableReason::UnsupportedProvider => "unsupported provider",
        EstimateUnavailableReason::UnsupportedModel => "unsupported model",
        EstimateUnavailableReason::UnknownTier => "unknown tier",
        EstimateUnavailableReason::UnsupportedTier => "unsupported tier",
        EstimateUnavailableReason::UnknownRequestGranularity => {
            "aggregate or unknown request granularity"
        }
        EstimateUnavailableReason::IncompleteCacheDetail => "incomplete cache detail",
        EstimateUnavailableReason::ArithmeticOverflow => "arithmetic overflow",
    }
}

fn model_label(attribution: &ModelAttribution) -> String {
    format!(
        "{} / {}",
        one_line(&attribution.provider),
        one_line(&attribution.model)
    )
}

fn group_label(group: &SummaryGroup) -> String {
    match group {
        SummaryGroup::ProviderModel(attribution) => model_label(attribution),
        SummaryGroup::Unattributed(UsageKind::Other) => "Unattributed other usage".into(),
        SummaryGroup::Unattributed(UsageKind::Assistant) => "Unattributed assistants".into(),
        SummaryGroup::Unattributed(UsageKind::ToolResult) => "Unattributed tool results".into(),
        SummaryGroup::Unattributed(UsageKind::Compaction) => "Unattributed compactions".into(),
        SummaryGroup::Unattributed(UsageKind::BranchSummary) => {
            "Unattributed branch summaries".into()
        }
    }
}

fn one_line(value: &str) -> String {
    value
        .chars()
        .flat_map(|character| match character {
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            character if character.is_control() => format!("\\u{{{:x}}}", u32::from(character))
                .chars()
                .collect(),
            character => vec![character],
        })
        .collect()
}

fn format_integer(value: impl Into<u128>) -> String {
    let digits = value.into().to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    let first_group = digits.len() % 3;

    for (index, character) in digits.chars().enumerate() {
        if index != 0 && index % 3 == first_group {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_formatting_uses_thousands_separators() {
        assert_eq!(format_integer(0_u64), "0");
        assert_eq!(format_integer(12_u64), "12");
        assert_eq!(format_integer(1_234_u64), "1,234");
        assert_eq!(format_integer(12_345_678_u64), "12,345,678");
    }
}
