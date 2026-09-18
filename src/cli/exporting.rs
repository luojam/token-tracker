use std::fs::OpenOptions;
use std::io;
use std::path::Path;

use token_tracker::{ExportSink, ExportSnapshot, PublishError, SqliteExportStore};

use super::CliError;

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
    let mut sink = if created {
        SqliteExportStore::open(path)
    } else {
        SqliteExportStore::open_existing(path)
    }
    .map_err(PublishError::Destination)?;
    sink.publish(snapshot)?;
    Ok(())
}
