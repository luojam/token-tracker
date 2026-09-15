use std::{collections::BTreeMap, convert::Infallible};

use token_tracker::{ExportSink, ExportSnapshot, PublishError, PublishOutcome};

#[derive(Default)]
struct LocalSink(BTreeMap<String, ExportSnapshot>);

impl ExportSink for LocalSink {
    type Error = Infallible;

    fn publish(
        &mut self,
        snapshot: &ExportSnapshot,
    ) -> Result<PublishOutcome, PublishError<Self::Error>> {
        if let Some(current) = self.0.get(&snapshot.machine_id) {
            if snapshot.export_revision < current.export_revision {
                return Err(PublishError::StaleRevision {
                    incoming_revision: snapshot.export_revision,
                    published_revision: current.export_revision,
                });
            }
            if snapshot.export_revision == current.export_revision {
                return if snapshot == current {
                    Ok(PublishOutcome::AlreadyPublished)
                } else {
                    Err(PublishError::RevisionConflict {
                        revision: snapshot.export_revision,
                    })
                };
            }
        }
        self.0.insert(snapshot.machine_id.clone(), snapshot.clone());
        Ok(PublishOutcome::Published)
    }
}

#[test]
fn publication_replaces_one_machine_and_handles_retries_and_ordering() {
    let mut sink = LocalSink::default();
    let first: ExportSnapshot =
        serde_json::from_str(include_str!("../fixtures/export-example.json")).unwrap();
    let mut other = first.clone();
    other.machine_id = "other-machine".into();
    sink.publish(&other).unwrap();
    assert_eq!(sink.publish(&first).unwrap(), PublishOutcome::Published);
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
    assert_eq!(sink.0[&first.machine_id], first);

    newer.export_revision += 1;
    newer.exported_at_unix_ms -= 1;
    assert_eq!(sink.publish(&newer).unwrap(), PublishOutcome::Published);
    assert!(matches!(
        sink.publish(&first),
        Err(PublishError::StaleRevision { .. })
    ));
    assert_eq!(sink.0[&first.machine_id], newer);
    assert_eq!(sink.0[&other.machine_id], other);
}
