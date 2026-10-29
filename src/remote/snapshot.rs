use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};

use super::{Result, invalid, valid_machine_id};
use crate::application::{
    UsageReport, UsageSnapshot, build_usage_report, deduplicate_events, reporting::summarize_events,
};
use crate::domain::{
    AgentId, EstimateTotal, EstimateTotals, EstimatedCost, ModelAttribution, RecordedCost,
    ReportFilters, SummaryBreakdown, SummaryGroup, SummaryTotals, TokenCounts, UsageEvent,
    UsageKind, UsageSummary, export::UsdAmount,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteSnapshot {
    pub format_version: u32,
    pub machine_id: String,
    pub machine_name: Option<String>,
    pub generated_at_unix_ms: i64,
    pub rows: Vec<SnapshotRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRow {
    pub date: String,
    pub agent: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub usage_kind: Option<UsageKind>,
    pub tokens: TokenCounts,
    pub costs: SnapshotCosts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCosts {
    pub recorded_usd: Option<UsdAmount>,
    pub estimated_usd: Option<UsdAmount>,
    pub estimate_overflow: bool,
    pub estimate_candidates: u64,
    pub priced_events: u64,
    pub rate_snapshots: BTreeMap<String, String>,
}

pub fn build_snapshot(
    stored: &UsageSnapshot,
    machine_id: String,
    machine_name: Option<String>,
    generated_at_unix_ms: i64,
) -> Result<RemoteSnapshot> {
    let usage = deduplicate_events(stored)?;
    let mut days = BTreeMap::<String, Vec<&UsageEvent>>::new();

    for selected in &usage.events {
        let event = &selected.canonical.event;
        let date = DateTime::from_timestamp_millis(event.timestamp.as_unix_milliseconds())
            .ok_or_else(|| invalid("event timestamp is out of range"))?
            .format("%Y-%m-%d")
            .to_string();
        days.entry(date).or_default().push(event);
    }

    let mut rows = Vec::new();

    for (date, events) in days {
        for row in summarize_events(0, events)?.breakdown {
            let (provider, model, usage_kind) = match row.group {
                SummaryGroup::ProviderModel(attribution) => {
                    (Some(attribution.provider), Some(attribution.model), None)
                }
                SummaryGroup::Unattributed(kind) => (None, None, Some(kind)),
            };

            rows.push(SnapshotRow {
                date: date.clone(),
                agent: row.agent.to_string(),
                provider,
                model,
                usage_kind,
                tokens: row.tokens,
                costs: SnapshotCosts {
                    recorded_usd: row.recorded_cost.map(Into::into),
                    estimated_usd: match row.estimates.cost {
                        EstimateTotal::Available(value) => Some(value.into()),
                        _ => None,
                    },
                    estimate_overflow: row.estimates.cost == EstimateTotal::Overflow,
                    estimate_candidates: row.estimates.estimate_candidate_event_count,
                    priced_events: row.estimates.priced_event_count,
                    rate_snapshots: row.estimates.rate_snapshots,
                },
            });
        }
    }

    let snapshot = RemoteSnapshot {
        format_version: 1,
        machine_id,
        machine_name,
        generated_at_unix_ms,
        rows,
    };
    snapshot.validate()?;

    Ok(snapshot)
}

impl RemoteSnapshot {
    pub fn validate(&self) -> Result<()> {
        if self.format_version != 1 || !valid_machine_id(&self.machine_id) {
            return Err(invalid(
                "unsupported snapshot version or invalid machine ID",
            ));
        }

        if DateTime::from_timestamp_millis(self.generated_at_unix_ms).is_none() {
            return Err(invalid("invalid snapshot generation time"));
        }

        let mut keys = BTreeSet::new();

        for row in &self.rows {
            date_ms(&row.date)?;
            if row.agent.is_empty()
                || !matches!(
                    (&row.provider, &row.model, row.usage_kind),
                    (Some(_), Some(_), None) | (None, None, Some(_))
                )
            {
                return Err(invalid("invalid snapshot dimensions"));
            }

            if !keys.insert((
                &row.date,
                &row.agent,
                &row.provider,
                &row.model,
                row.usage_kind,
            )) {
                return Err(invalid("duplicate snapshot row"));
            }

            row.costs.decode()?;
        }

        Ok(())
    }
}

fn date_ms(value: &str) -> Result<i64> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| invalid("invalid snapshot date"))?;
    if date.format("%Y-%m-%d").to_string() != value {
        return Err(invalid("invalid snapshot date"));
    }

    Ok(date
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis())
}

impl SnapshotCosts {
    fn decode(&self) -> Result<(Option<RecordedCost>, EstimateTotals)> {
        if self.priced_events > self.estimate_candidates
            || (self.estimate_overflow && (self.priced_events == 0 || self.estimated_usd.is_some()))
            || (!self.estimate_overflow && self.estimated_usd.is_some() != (self.priced_events > 0))
        {
            return Err(invalid("inconsistent snapshot pricing counts"));
        }

        for (version, date) in &self.rate_snapshots {
            if version.is_empty() {
                return Err(invalid("empty pricing version"));
            }
            date_ms(date)?;
        }

        let recorded = self
            .recorded_usd
            .as_ref()
            .map(|value| {
                let value = value
                    .as_str()
                    .parse::<f64>()
                    .map_err(|_| invalid("invalid recorded cost"))?;
                RecordedCost::from_usd(value).map_err(|_| invalid("invalid recorded cost"))
            })
            .transpose()?;

        let cost = if self.estimate_overflow {
            EstimateTotal::Overflow
        } else if let Some(value) = &self.estimated_usd {
            EstimateTotal::Available(parse_estimated(value)?)
        } else {
            EstimateTotal::Unavailable
        };

        Ok((
            recorded,
            EstimateTotals {
                cost,
                estimate_candidate_event_count: self.estimate_candidates,
                priced_event_count: self.priced_events,
                rate_snapshots: self.rate_snapshots.clone(),
                ..Default::default()
            },
        ))
    }
}

fn parse_estimated(value: &UsdAmount) -> Result<EstimatedCost> {
    let (whole, fraction) = value
        .as_str()
        .split_once('.')
        .unwrap_or((value.as_str(), ""));
    if fraction.len() > 12 {
        return Err(invalid("estimate exceeds picodollar precision"));
    }

    let whole = whole
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(1_000_000_000_000));
    let fraction = format!("{fraction:0<12}").parse::<u128>().ok();

    whole
        .zip(fraction)
        .and_then(|(a, b)| a.checked_add(b))
        .map(EstimatedCost::from_picodollars)
        .ok_or_else(|| invalid("estimated cost overflow"))
}

pub fn combine_snapshots(
    snapshots: &[RemoteSnapshot],
    range: Option<Range<i64>>,
    filters: &ReportFilters,
) -> Result<UsageReport> {
    let mut machines = BTreeSet::new();
    let mut summary = UsageSummary::default();
    let mut groups = BTreeMap::<(AgentId, SummaryGroup), SummaryTotals>::new();

    for snapshot in snapshots {
        snapshot.validate()?;
        if !machines.insert(&snapshot.machine_id) {
            return Err(invalid("duplicate machine snapshot"));
        }

        for row in &snapshot.rows {
            if range
                .as_ref()
                .is_some_and(|range| !range.contains(&date_ms(&row.date).unwrap()))
                || !filters.matches_dimensions(
                    &row.agent,
                    row.provider.as_deref(),
                    row.model.as_deref(),
                )
            {
                continue;
            }

            let group = match (&row.provider, &row.model) {
                (Some(provider), Some(model)) => SummaryGroup::ProviderModel(ModelAttribution {
                    provider: provider.clone(),
                    model: model.clone(),
                }),
                _ => SummaryGroup::Unattributed(row.usage_kind.unwrap()),
            };

            merge(&mut summary.totals, row)?;
            merge(
                groups.entry((row.agent.clone().into(), group)).or_default(),
                row,
            )?;
        }
    }

    summary.breakdown = groups
        .into_iter()
        .map(|((agent, group), totals)| SummaryBreakdown {
            agent,
            group,
            tokens: totals.tokens,
            recorded_cost: totals.recorded_cost,
            estimates: totals.estimates,
            unique_usage_event_count: 0,
        })
        .collect();

    Ok(build_usage_report(&summary))
}

fn merge(totals: &mut SummaryTotals, row: &SnapshotRow) -> Result<()> {
    totals.tokens = totals
        .tokens
        .checked_add(row.tokens)
        .ok_or_else(|| invalid("token total overflow"))?;

    let (recorded, estimates) = row.costs.decode()?;
    if let Some(value) = recorded {
        totals.recorded_cost = Some(match totals.recorded_cost {
            Some(current) => current
                .checked_add(value)
                .map_err(|_| invalid("recorded cost overflow"))?,
            None => value,
        });
    }

    let target = &mut totals.estimates;
    target.estimate_candidate_event_count = target
        .estimate_candidate_event_count
        .checked_add(estimates.estimate_candidate_event_count)
        .ok_or_else(|| invalid("pricing count overflow"))?;
    target.priced_event_count = target
        .priced_event_count
        .checked_add(estimates.priced_event_count)
        .ok_or_else(|| invalid("pricing count overflow"))?;

    target.cost = match (target.cost, estimates.cost) {
        (EstimateTotal::Overflow, _) | (_, EstimateTotal::Overflow) => EstimateTotal::Overflow,
        (EstimateTotal::Unavailable, other) | (other, EstimateTotal::Unavailable) => other,
        (EstimateTotal::Available(a), EstimateTotal::Available(b)) => a
            .checked_add(b)
            .map(EstimateTotal::Available)
            .unwrap_or(EstimateTotal::Overflow),
    };

    for (version, date) in estimates.rate_snapshots {
        if target
            .rate_snapshots
            .get(&version)
            .is_some_and(|existing| existing != &date)
        {
            return Err(invalid("conflicting pricing version dates"));
        }
        target.rate_snapshots.insert(version, date);
    }

    Ok(())
}
