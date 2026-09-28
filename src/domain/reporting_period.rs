use chrono::{DateTime, Datelike, Duration, Utc};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportingPeriod {
    #[default]
    AllTime,
    Day,
    Week,
    Month,
}

impl ReportingPeriod {
    pub fn label(self) -> &'static str {
        match self {
            Self::AllTime => "All Time",
            Self::Day => "Today (UTC)",
            Self::Week => "This Week (UTC)",
            Self::Month => "This Month (UTC)",
        }
    }

    pub fn current_range(self) -> Option<std::ops::Range<i64>> {
        let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let now = DateTime::from_timestamp(elapsed.as_secs() as i64, elapsed.subsec_nanos())
            .expect("current time is in range");
        self.range_at(now)
    }

    /// UTC calendar boundaries, with Monday as the first day of the week.
    pub fn range_at(self, now: DateTime<Utc>) -> Option<std::ops::Range<i64>> {
        let date = now.date_naive();
        let start = match self {
            Self::AllTime => return None,
            Self::Day => date,
            Self::Week => date - Duration::days(date.weekday().num_days_from_monday().into()),
            Self::Month => date.with_day(1).unwrap(),
        };
        let end = match self {
            Self::Day => start + Duration::days(1),
            Self::Week => start + Duration::days(7),
            Self::Month => (start + Duration::days(32)).with_day(1).unwrap(),
            Self::AllTime => unreachable!(),
        };
        let milliseconds = |date: chrono::NaiveDate| {
            date.and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        };
        Some(milliseconds(start)..milliseconds(end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_ranges_cover_leap_days_and_weeks_crossing_years() {
        let timestamp = |value: &str| value.parse::<DateTime<Utc>>().unwrap();
        for (period, now, start, end) in [
            (
                ReportingPeriod::Day,
                "2024-02-29T12:00:00Z",
                "2024-02-29T00:00:00Z",
                "2024-03-01T00:00:00Z",
            ),
            (
                ReportingPeriod::Week,
                "2025-01-05T23:59:59Z",
                "2024-12-30T00:00:00Z",
                "2025-01-06T00:00:00Z",
            ),
            (
                ReportingPeriod::Week,
                "2025-01-06T00:00:00Z",
                "2025-01-06T00:00:00Z",
                "2025-01-13T00:00:00Z",
            ),
            (
                ReportingPeriod::Month,
                "2024-02-29T12:00:00Z",
                "2024-02-01T00:00:00Z",
                "2024-03-01T00:00:00Z",
            ),
        ] {
            assert_eq!(
                period.range_at(timestamp(now)),
                Some(timestamp(start).timestamp_millis()..timestamp(end).timestamp_millis())
            );
        }
    }
}
