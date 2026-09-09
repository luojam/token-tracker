use std::fmt::Write;

use super::ImportWarning;
use crate::core::{
    EstimateTotal, EstimateTotals, EstimatedCost, ModelAttribution, RecordedCost, SummaryGroup,
    UsageKind, UsageSummary,
};

pub fn render_terminal_report(summary: &UsageSummary, warnings: &[ImportWarning]) -> String {
    let mut output = String::new();
    let totals = &summary.totals;

    writeln!(output, "Token Tracker — All Time").unwrap();
    writeln!(output).unwrap();
    writeln!(
        output,
        "Total tokens: {}",
        format_integer(totals.tokens.total())
    )
    .unwrap();
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
    if let Some(cost) = cost_label(
        totals.recorded_cost,
        summary.estimate.iter().map(|estimate| &estimate.totals),
    ) {
        writeln!(output, "Total cost: {cost}").unwrap();
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
        let headers = [
            "Provider / model",
            "Input",
            "Output",
            "Cache read",
            "Cache write",
            "Total",
            "Events",
            "Cost",
        ]
        .map(str::to_owned);
        let rows = summary
            .breakdown
            .iter()
            .map(|row| {
                let estimates = summary
                    .estimate
                    .iter()
                    .filter(|_| row.agent.as_str() == "codex")
                    .flat_map(|estimate| &estimate.breakdown)
                    .filter(|estimate| match &row.group {
                        SummaryGroup::ProviderModel(attribution) => {
                            estimate.attribution.as_ref() == Some(attribution)
                        }
                        SummaryGroup::Unattributed(_) => false,
                    })
                    .map(|estimate| &estimate.totals);
                (
                    &row.agent,
                    [
                        group_label(&row.group),
                        format_integer(row.tokens.input),
                        format_integer(row.tokens.output),
                        format_integer(row.tokens.cache_read),
                        format_integer(row.tokens.cache_write),
                        format_integer(row.tokens.total()),
                        format_integer(row.unique_usage_event_count),
                        cost_label(row.recorded_cost, estimates).unwrap_or_else(|| "-".into()),
                    ],
                )
            })
            .collect::<Vec<_>>();
        let mut widths = headers.each_ref().map(|header| header.chars().count());
        for (_, cells) in &rows {
            for (width, cell) in widths.iter_mut().zip(cells) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let mut previous_agent = None;
        for (agent, cells) in &rows {
            if previous_agent != Some(agent) {
                let label = match agent.as_str() {
                    "pi" => "Pi".into(),
                    "codex" => "Codex".into(),
                    agent => one_line(agent),
                };
                writeln!(output).unwrap();
                writeln!(output, "{label} usage:").unwrap();
                render_table_row(&mut output, &headers, &widths);
                previous_agent = Some(agent);
            }
            render_table_row(&mut output, cells, &widths);
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

fn render_table_row(output: &mut String, cells: &[String; 8], widths: &[usize; 8]) {
    for (index, (cell, width)) in cells.iter().zip(widths).enumerate() {
        if index == 0 {
            write!(output, "  {cell:<width$}").unwrap();
        } else {
            write!(output, "  {cell:>width$}").unwrap();
        }
    }
    writeln!(output).unwrap();
}

fn cost_label<'a>(
    recorded: Option<RecordedCost>,
    estimates: impl Iterator<Item = &'a EstimateTotals>,
) -> Option<String> {
    let mut estimated = None;
    let mut has_estimates = false;
    let mut partial = false;
    for totals in estimates {
        has_estimates = true;
        partial |= totals.priced_event_count < totals.imported_event_count;
        match totals.cost {
            EstimateTotal::Available(cost) => {
                let current = estimated.unwrap_or(EstimatedCost::default());
                let Some(combined) = current.checked_add(cost) else {
                    return Some("unavailable (arithmetic overflow)".into());
                };
                estimated = Some(combined);
            }
            EstimateTotal::Unavailable => {}
            EstimateTotal::Overflow => return Some("unavailable (arithmetic overflow)".into()),
        }
    }
    let mut label = match (recorded, estimated) {
        (Some(recorded), estimated) => {
            let cost = recorded.as_usd()
                + estimated.map_or(0.0, |cost| cost.as_picodollars() as f64 / 1e12);
            if !cost.is_finite() {
                return Some("unavailable (arithmetic overflow)".into());
            }
            if cost > 0.0 && cost < 0.000001 {
                "<$0.000001".into()
            } else {
                format!("${cost:.6}")
            }
        }
        (None, Some(estimated)) => estimated.to_string(),
        (None, None) if has_estimates => return Some("unavailable".into()),
        (None, None) => return None,
    };
    if partial {
        label.push_str(" (partial)");
    }
    Some(label)
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
