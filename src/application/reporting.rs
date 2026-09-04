//! Deterministic terminal rendering for all-time usage summaries.

use std::fmt::Write;

use super::ImportWarning;
use crate::core::{SummaryGroup, UsageKind, UsageSummary};

/// Renders an all-time summary and any nonfatal import warnings.
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

fn group_label(group: &SummaryGroup) -> String {
    match group {
        SummaryGroup::ProviderModel(attribution) => format!(
            "{} / {}",
            one_line(&attribution.provider),
            one_line(&attribution.model)
        ),
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
