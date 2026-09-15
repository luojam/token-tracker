use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use token_tracker::{ExportSink, ExportSnapshot, PublishError, SqliteExportSink};

use super::CliError;

const APPLICATION_ID: i32 = 0x54544558;

pub(super) fn write_snapshot(
    path: &Path,
    force: bool,
    snapshot: &ExportSnapshot,
) -> Result<(), CliError> {
    let output_error = |source| CliError::ExportOutput {
        path: path.to_owned(),
        source,
    };
    let path = std::path::absolute(path).map_err(output_error)?;
    let created = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(_) => true,
        Err(error) if force && error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(output_error(error)),
    };
    write_database(&path, created, snapshot)
        .map_err(|source| CliError::ExportDatabase { path, source })
}

fn write_database(
    path: &Path,
    created: bool,
    snapshot: &ExportSnapshot,
) -> Result<(), PublishError<rusqlite::Error>> {
    if !created {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(PublishError::Destination)?;
        let application_id: i32 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(PublishError::Destination)?;
        if application_id != APPLICATION_ID {
            return Err(PublishError::Destination(
                rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "destination is not a token-tracker export database",
                ))),
            ));
        }
    }
    let mut sink = SqliteExportSink::open(path).map_err(PublishError::Destination)?;
    sink.publish(snapshot)?;
    Ok(())
}
