use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use token_tracker::adapters::codex::{CodexSessionDiscovery, CodexSessionParser};
use token_tracker::application::{
    SynchronizationReport, UsageReadStore, UsageSnapshot, UsageStore, synchronize_sessions_at,
};
use token_tracker::domain::{AgentId, Timestamp};
use token_tracker::storage::SqliteUsageStore;

static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

struct Ledger {
    store: SqliteUsageStore,
    root: PathBuf,
    clock: i64,
}

impl Ledger {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "token-tracker-codex-lifecycle-{}-{}",
            std::process::id(),
            NEXT_TREE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            store: SqliteUsageStore::open_in_memory().unwrap(),
            root,
            clock: 0,
        }
    }

    fn sync(&mut self, source: &str) -> SynchronizationReport {
        fs::write(self.root.join("rollout-active.jsonl"), source).unwrap();
        self.clock += 1;
        synchronize_sessions_at(
            &CodexSessionDiscovery::new([self.root.clone()]),
            &CodexSessionParser::new(),
            &mut self.store,
            Timestamp::from_unix_milliseconds(self.clock),
        )
        .unwrap()
    }

    fn snapshot(&self) -> UsageSnapshot {
        self.store.usage_snapshot().unwrap()
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn fixture(name: &str) -> String {
    fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/codex")
            .join(format!("{name}.jsonl")),
    )
    .unwrap()
}

fn prefix(source: &str, lines: usize) -> String {
    source.split_inclusive('\n').take(lines).collect()
}

fn imported(report: &SynchronizationReport, inserted: u64, updated: u64) {
    assert!(report.warnings.is_empty(), "{report:?}");
    assert_eq!(report.counts.files_imported, 1, "{report:?}");
    assert_eq!(
        report.counts.event_identities_inserted, inserted,
        "{report:?}"
    );
    assert_eq!(report.counts.observations_inserted, inserted, "{report:?}");
    assert_eq!(report.counts.observations_updated, updated, "{report:?}");
}

#[test]
fn upgrade_prefixes_and_partial_mirrors_preserve_imported_keys() {
    let mut ledger = Ledger::new();
    let source = fixture("upgrade-response-first");
    imported(&ledger.sync(&prefix(&source, 8)), 1, 0);
    let legacy = ledger.snapshot().observations[0].clone();
    assert_eq!(
        legacy.event.identity.adapter_key,
        "legacy-turn-v1:turn-legacy-a"
    );

    imported(&ledger.sync(&prefix(&source, 12)), 1, 0);
    let original = ledger.snapshot();
    assert_eq!(original.observations.len(), 2);
    assert!(original.observations.contains(&legacy));
    assert!(
        original.observations.iter().any(|observation| {
            observation.event.identity.adapter_key == "response-v1:response-a"
        })
    );

    let cut = source.trim_end().len() - 1;
    let partial = ledger.sync(&source[..cut]);
    imported(&partial, 0, 0);
    assert_eq!(partial.counts.incomplete_files_imported, 1);
    assert_eq!(ledger.snapshot(), original);
    let completed = ledger.sync(&source);
    imported(&completed, 0, 0);
    assert_eq!(completed.counts.incomplete_files_imported, 0);
    assert_eq!(ledger.snapshot(), original);
}

#[test]
fn rejected_mirror_offset_retains_the_last_valid_import() {
    let mut ledger = Ledger::new();
    let source = fixture("reject-mirror-offset");
    imported(&ledger.sync(&prefix(&source, 12)), 2, 0);
    let original = ledger.snapshot();
    assert_eq!(original.observations.len(), 2);
    let last_import = ledger.store.source_states(&AgentId::from("codex")).unwrap()[0]
        .last_import
        .clone();
    let rejected = ledger.sync(&source);
    assert_eq!(rejected.counts.files_failed, 1, "{rejected:?}");
    assert_eq!(rejected.counts.files_imported, 0);
    assert_eq!(ledger.snapshot(), original);
    let states = ledger.store.source_states(&AgentId::from("codex")).unwrap();
    assert_eq!(states[0].last_import, last_import);
    assert_ne!(
        Some(&states[0].last_observed_revision),
        last_import.as_ref().map(|import| &import.revision)
    );
}
