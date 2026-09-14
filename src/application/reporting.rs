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
