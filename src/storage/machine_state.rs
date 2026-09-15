use std::{path::Path, time::Duration};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use super::SqliteStoreError;

const APPLICATION_ID: i64 = 0x54544d53;

pub(crate) struct MachineState {
    connection: Connection,
}

pub(crate) struct MachineExport<'a> {
    transaction: Transaction<'a>,
    pub machine_id: String,
    pub revision: u64,
}

impl MachineState {
    pub fn open(path: &Path) -> Result<Self, SqliteStoreError> {
        let connection = Connection::open(path)?;
        if connection.path() == Some("") {
            return Err(SqliteStoreError::InvalidMachineStatePath(path.to_owned()));
        }
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    /// Hold the lock through the usage read so revision order follows snapshot order.
    pub fn begin_export(&mut self) -> Result<MachineExport<'_>, SqliteStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let application_id: i64 =
            transaction.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if application_id == 0 && version == 0 {
            let tables: i64 = transaction.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )?;
            if tables != 0 {
                return Err(SqliteStoreError::CorruptData(
                    "an invalid machine state schema",
                ));
            }
            transaction.execute_batch(
                "CREATE TABLE machine_state (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    machine_id TEXT NOT NULL CHECK (length(machine_id) = 32),
                    export_revision TEXT NOT NULL
                );
                INSERT INTO machine_state VALUES (1, lower(hex(randomblob(16))), '0');",
            )?;
            transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
            transaction.pragma_update(None, "user_version", 1)?;
        } else if application_id != APPLICATION_ID || version != 1 {
            return Err(SqliteStoreError::CorruptData(
                "an incompatible machine state schema",
            ));
        }
        let (machine_id, revision): (String, String) = transaction.query_row(
            "SELECT machine_id, export_revision FROM machine_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if machine_id.len() != 32 || !machine_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(SqliteStoreError::CorruptData("an invalid machine identity"));
        }
        let revision = revision
            .parse::<u64>()
            .map_err(|_| SqliteStoreError::CorruptData("an invalid export revision"))?
            .checked_add(1)
            .ok_or(SqliteStoreError::ValueOutOfRange("export revision"))?;
        Ok(MachineExport {
            transaction,
            machine_id,
            revision,
        })
    }
}

impl MachineExport<'_> {
    pub fn commit(self) -> Result<(), SqliteStoreError> {
        self.transaction.execute(
            "UPDATE machine_state SET export_revision = ?1 WHERE singleton = 1",
            params![self.revision.to_string()],
        )?;
        self.transaction.commit()?;
        Ok(())
    }
}
