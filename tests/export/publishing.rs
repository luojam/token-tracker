use token_tracker::{ExportSink, ExportSnapshot, PublishError, PublishOutcome, SqliteExportSink};

use crate::support::TempTree;

#[test]
fn publication_replaces_one_machine_and_handles_retries_and_ordering() {
    let tree = TempTree::new();
    let path = tree.root.join("export.db");
    let mut sink = SqliteExportSink::open(&path).unwrap();
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
    let mut sink = SqliteExportSink::open(&path).unwrap();
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
