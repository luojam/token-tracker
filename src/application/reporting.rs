use crate::domain::{
    AgentId, EstimateTotal, EstimateTotals, EstimatedCost, RecordedCost, SummaryGroup, TokenCounts,
    UsageSummary,
};

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
    pub session_count: u64,
    pub unique_usage_event_count: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReportRow {
    pub agent: AgentId,
    pub group: SummaryGroup,
    pub tokens: TokenCounts,
    pub cost: CostTotal,
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
        cost: aggregate_cost(
            summary.totals.recorded_cost,
            summary.estimates.values().map(|estimate| &estimate.totals),
        ),
        session_count: summary.totals.session_count,
        unique_usage_event_count: summary.totals.unique_usage_event_count,
    };
    let rows = summary
        .breakdown
        .iter()
        .map(|row| {
            let estimates = summary
                .estimates
                .get(&row.agent)
                .into_iter()
                .flat_map(|estimate| &estimate.breakdown)
                .filter(|estimate| estimate.group == row.group)
                .map(|estimate| &estimate.totals);
            ReportRow {
                agent: row.agent.clone(),
                group: row.group.clone(),
                tokens: row.tokens,
                cost: aggregate_cost(row.recorded_cost, estimates),
                unique_usage_event_count: row.unique_usage_event_count,
            }
        })
        .collect();
    UsageReport { totals, rows }
}

fn aggregate_cost<'a>(
    recorded: Option<RecordedCost>,
    estimates: impl Iterator<Item = &'a EstimateTotals>,
) -> CostTotal {
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
                    return CostTotal::Overflow;
                };
                estimated = Some(combined);
            }
            EstimateTotal::Unavailable => {}
            EstimateTotal::Overflow => return CostTotal::Overflow,
        }
    }
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
        (None, None) if has_estimates => return CostTotal::Unavailable,
        (None, None) => return CostTotal::Absent,
    };
    CostTotal::Available { amount, partial }
}
