use token_tracker::storage::SqliteUsageStore;
use token_tracker::{ExportSink, ExportSnapshot, PublishError, PublishOutcome, SqliteExportStore};

use crate::support::TempTree;

#[test]
fn existing_export_policy_rejects_missing_and_empty_files() {
    let tree = TempTree::new();
    let path = tree.root.join("export.db");
    assert!(SqliteExportStore::open_existing(&path).is_err());
    assert!(!path.exists());

    std::fs::write(&path, []).unwrap();
    assert!(SqliteExportStore::open_existing(&path).is_err());
    assert!(std::fs::read(&path).unwrap().is_empty());

    drop(SqliteExportStore::open(&path).unwrap());
    assert!(SqliteExportStore::open_existing(&path).is_ok());
}

#[test]
fn usage_and_export_databases_cannot_be_opened_as_each_other() {
    let tree = TempTree::new();
    let usage = tree.root.join("usage.db");
    drop(SqliteUsageStore::open(&usage).unwrap());
    let before = std::fs::read(&usage).unwrap();
    assert!(SqliteExportStore::open(&usage).is_err());
    assert!(SqliteExportStore::open_existing(&usage).is_err());
    assert_eq!(std::fs::read(&usage).unwrap(), before);

    let export = tree.root.join("export.db");
    drop(SqliteExportStore::open(&export).unwrap());
    let before = std::fs::read(&export).unwrap();
    assert!(SqliteUsageStore::open(&export).is_err());
    assert_eq!(std::fs::read(&export).unwrap(), before);
}

#[test]
fn publication_replaces_one_machine_and_handles_retries_and_ordering() {
    let tree = TempTree::new();
    let path = tree.root.join("export.db");
    let mut sink = SqliteExportStore::open(&path).unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    let stored = |machine: &str| -> ExportSnapshot {
        let payload: String = connection
            .query_row(
                "SELECT payload FROM snapshot WHERE machine_id = ?1",
                [machine],
                |row| row.get(0),
            )
            .unwrap();
        serde_json::from_str(&payload).unwrap()
    };
    let first: ExportSnapshot =
        serde_json::from_str(include_str!("../fixtures/export-example.json")).unwrap();
    let mut other = first.clone();
    other.machine_id = "other-machine".into();
    sink.publish(&other).unwrap();
    assert_eq!(sink.publish(&first).unwrap(), PublishOutcome::Published);
    drop(sink);
    let mut sink = SqliteExportStore::open(&path).unwrap();
    assert_eq!(
        sink.publish(&first).unwrap(),
        PublishOutcome::AlreadyPublished
    );

    let mut newer = first.clone();
    newer.machine_name = Some("Renamed".into());
    newer.events.clear();
    assert!(matches!(
        sink.publish(&newer),
        Err(PublishError::RevisionConflict { .. })
    ));
    assert_eq!(stored(&first.machine_id), first);

    let mut invalid = first.clone();
    invalid.export_revision += 1;
    invalid.events.push(invalid.events[0].clone());
    assert!(matches!(
        sink.publish(&invalid),
        Err(PublishError::InvalidSnapshot { .. })
    ));
    assert_eq!(stored(&first.machine_id), first);

    newer.export_revision = u64::MAX;
    newer.exported_at_unix_ms -= 1;
    assert_eq!(sink.publish(&newer).unwrap(), PublishOutcome::Published);
    assert!(matches!(
        sink.publish(&first),
        Err(PublishError::StaleRevision { .. })
    ));
    assert_eq!(stored(&first.machine_id), newer);
    assert_eq!(stored(&other.machine_id), other);
    let machines: Vec<String> = connection
        .prepare("SELECT machine_id FROM events")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(machines, vec![other.machine_id]);
}
