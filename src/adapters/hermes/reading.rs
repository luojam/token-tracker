use std::{collections::BTreeMap, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde::Serialize;

use super::{HermesDatabaseSnapshot, HermesReadError, HermesSessionSnapshot, parsing};
use crate::application::SourceKey;

const SESSION_REQUIRED: &[&str] = &[
    "id",
    "started_at",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
    "api_call_count",
];
const SESSION_OPTIONAL: &[&str] = &[
    "cwd",
    "title",
    "parent_session_id",
    "billing_provider",
    "billing_base_url",
    "billing_mode",
    "estimated_cost_usd",
    "actual_cost_usd",
    "cost_status",
    "cost_source",
    "pricing_version",
];
const USAGE_REQUIRED: &[&str] = &[
    "session_id",
    "model",
    "billing_provider",
    "billing_base_url",
    "billing_mode",
    "task",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "reasoning_tokens",
    "api_call_count",
    "first_seen",
    "last_seen",
];
const USAGE_OPTIONAL: &[&str] = &[
    "estimated_cost_usd",
    "actual_cost_usd",
    "cost_status",
    "cost_source",
];

/// Reads committed accounting, including WAL, then closes the transaction before normalization.
/// The caller must supply a live database or a consistent SQLite backup.
pub fn read_snapshot(path: &Path) -> Result<HermesDatabaseSnapshot, HermesReadError> {
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(2))?;

    let transaction = connection.transaction()?;
    let session_columns = columns(&transaction, "sessions", SESSION_REQUIRED, SESSION_OPTIONAL)?;
    let usage_columns = columns(
        &transaction,
        "session_model_usage",
        USAGE_REQUIRED,
        USAGE_OPTIONAL,
    )?;

    let mut sessions = BTreeMap::new();
    for row in read_rows(&transaction, "sessions", &session_columns)? {
        let id = row.text("id")?;
        if id.is_empty() {
            return Err(HermesReadError::InvalidField("id"));
        }
        if sessions.insert(id.to_owned(), (row, Vec::new())).is_some() {
            return Err(HermesReadError::InconsistentAccounting(
                "duplicate session ID",
            ));
        }
    }

    for row in read_rows(&transaction, "session_model_usage", &usage_columns)? {
        let id = row.text("session_id")?;
        let Some((_, usage)) = sessions.get_mut(id) else {
            return Err(HermesReadError::InconsistentAccounting(
                "usage references a missing session",
            ));
        };
        usage.push(row);
    }

    transaction.commit()?;
    drop(connection);

    Ok(HermesDatabaseSnapshot {
        sessions: sessions
            .into_iter()
            .map(|(id, (session, mut usage))| {
                usage.sort();
                HermesSessionSnapshot {
                    key: SourceKey(
                        serde_json::to_vec(&("hermes-session-v1", &id)).expect("string identity"),
                    ),
                    snapshot: parsing::normalize(&session, &usage),
                }
            })
            .collect(),
    })
}

fn columns(
    connection: &Connection,
    table: &'static str,
    required: &'static [&'static str],
    optional: &'static [&'static str],
) -> Result<Vec<&'static str>, HermesReadError> {
    let mut statement = connection.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let available = statement
        .query_map([table], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let missing: Vec<_> = required
        .iter()
        .copied()
        .filter(|column| !available.iter().any(|name| name == column))
        .collect();
    if !missing.is_empty() {
        return Err(HermesReadError::UnsupportedSchema { table, missing });
    }

    Ok(required
        .iter()
        .chain(
            optional
                .iter()
                .filter(|column| available.iter().any(|name| name == **column)),
        )
        .copied()
        .collect())
}

fn read_rows(
    connection: &Connection,
    table: &str,
    columns: &[&'static str],
) -> Result<Vec<AccountingRow>, HermesReadError> {
    // Table and column names come only from the fixed accounting allowlists above.
    let mut statement =
        connection.prepare(&format!("SELECT {} FROM {table}", columns.join(", ")))?;
    let rows = statement.query_map([], |row| {
        columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let value = match row.get_ref(index)? {
                    ValueRef::Null => Field::Null,
                    ValueRef::Integer(value) => Field::Integer(value),
                    ValueRef::Real(value) => Field::Real(value.to_bits()),
                    ValueRef::Text(value) => Field::Text(value.to_vec()),
                    ValueRef::Blob(value) => Field::Blob(value.to_vec()),
                };
                Ok((*column, value))
            })
            .collect::<Result<BTreeMap<_, _>, rusqlite::Error>>()
            .map(AccountingRow)
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

// Preserve SQLite types and float bits for revisions, including unrecognized cost evidence.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
enum Field {
    Null,
    Integer(i64),
    Real(u64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct AccountingRow(BTreeMap<&'static str, Field>);

impl AccountingRow {
    pub fn text(&self, field: &'static str) -> Result<&str, HermesReadError> {
        match self.0.get(field) {
            Some(Field::Text(bytes)) => {
                std::str::from_utf8(bytes).map_err(|_| HermesReadError::InvalidField(field))
            }
            _ => Err(HermesReadError::InvalidField(field)),
        }
    }

    pub fn optional_text(&self, field: &'static str) -> Option<&str> {
        self.text(field)
            .ok()
            .filter(|value| !value.trim().is_empty())
    }

    pub fn counter(&self, field: &'static str) -> Result<u64, HermesReadError> {
        match self.0.get(field) {
            Some(Field::Integer(value)) if *value >= 0 => Ok(*value as u64),
            _ => Err(HermesReadError::InvalidField(field)),
        }
    }

    pub fn number(&self, field: &'static str) -> Option<f64> {
        match self.0.get(field) {
            Some(Field::Integer(value)) => Some(*value as f64),
            Some(Field::Real(bits)) => Some(f64::from_bits(*bits)),
            _ => None,
        }
    }

    pub fn timestamp(
        &self,
        field: &'static str,
    ) -> Result<Option<crate::domain::Timestamp>, HermesReadError> {
        let milliseconds = match self.0.get(field) {
            Some(Field::Null) => return Ok(None),
            Some(Field::Integer(seconds)) => seconds.checked_mul(1000),
            Some(Field::Real(bits)) => {
                let value = f64::from_bits(*bits) * 1000.0;
                if value.is_finite() && value >= i64::MIN as f64 && value < -(i64::MIN as f64) {
                    Some(value.trunc() as i64)
                } else {
                    None
                }
            }
            _ => None,
        };
        milliseconds
            .map(crate::domain::Timestamp::from_unix_milliseconds)
            .map(Some)
            .ok_or(HermesReadError::InvalidField(field))
    }
}
