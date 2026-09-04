use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use token_tracker::adapters::pi::{PiParseError, PiSessionDiscovery, PiSessionParser};
use token_tracker::adapters::sqlite::SqliteUsageStore;
use token_tracker::application::{
    ParsedSession, SessionParser, UsageStore, synchronize_sessions_at,
};
use token_tracker::core::Timestamp;

static NEXT_TEMP_TREE: AtomicU64 = AtomicU64::new(0);

struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new() -> Self {
        let sequence = NEXT_TEMP_TREE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "token-tracker-import-test-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        Self { root }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn scan_time(value: i64) -> Timestamp {
    Timestamp::from_unix_milliseconds(value)
}

fn header(session_id: &str, parent: Option<&Path>) -> String {
    let parent = parent
        .map(|path| format!(",\"parentSession\":{:?}", path.to_string_lossy()))
        .unwrap_or_default();
    format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{session_id}\",\"timestamp\":\"2025-01-02T03:04:05.000Z\",\"cwd\":\"/work/project\"{parent}}}\n"
    )
}

fn assistant_event(event_id: &str, input: u64) -> String {
    format!(
        "{{\"type\":\"message\",\"id\":\"{event_id}\",\"timestamp\":\"2025-01-02T03:05:00.000Z\",\"message\":{{\"role\":\"assistant\",\"provider\":\"provider\",\"model\":\"model\",\"usage\":{{\"input\":{input},\"output\":2,\"cacheRead\":3,\"cacheWrite\":4}}}}}}\n"
    )
}

fn session(session_id: &str, parent: Option<&Path>, events: &[(&str, u64)]) -> String {
    let mut value = header(session_id, parent);
    for (event_id, input) in events {
        value.push_str(&assistant_event(event_id, *input));
    }
    value
}

fn synchronize(
    root: &Path,
    store: &mut SqliteUsageStore,
    time: i64,
) -> token_tracker::application::SynchronizationReport {
    synchronize_sessions_at(
        &PiSessionDiscovery::new(root),
        &PiSessionParser::new(),
        store,
        scan_time(time),
    )
    .unwrap()
}

#[test]
fn repeat_append_rewrite_parse_failure_and_missing_source_are_synchronized() {
    let tree = TempTree::new();
    let path = tree.root.join("session.jsonl");
    fs::write(&path, session("session-a", None, &[("event-a", 10)])).unwrap();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    let first = synchronize(&tree.root, &mut store, 1_000);
    assert_eq!(first.counts.files_imported, 1);
    assert_eq!(first.counts.event_identities_inserted, 1);
    assert_eq!(first.counts.observations_inserted, 1);

    let repeated = synchronize(&tree.root, &mut store, 2_000);
    assert_eq!(repeated.counts.files_unchanged, 1);
    assert_eq!(repeated.counts.files_imported, 0);

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(assistant_event("event-b", 20).as_bytes())
        .unwrap();
    let appended = synchronize(&tree.root, &mut store, 3_000);
    assert_eq!(appended.counts.files_imported, 1);
    assert_eq!(appended.counts.event_identities_inserted, 1);
    assert_eq!(appended.counts.observations_inserted, 1);
    assert_eq!(appended.counts.observations_updated, 0);

    // A full rewrite updates observations still present without deleting usage
    // that is absent from the new file.
    fs::write(&path, session("session-a", None, &[("event-a", 999_999)])).unwrap();
    let rewritten = synchronize(&tree.root, &mut store, 4_000);
    assert_eq!(rewritten.counts.files_imported, 1);
    assert_eq!(rewritten.counts.event_identities_inserted, 0);
    assert_eq!(rewritten.counts.observations_inserted, 0);
    assert_eq!(rewritten.counts.observations_updated, 1);
    let last_good_revision = store.source_states().unwrap()[0]
        .last_imported_revision
        .clone();

    fs::write(
        &path,
        format!("{}{{malformed complete line}}\n", header("session-a", None)),
    )
    .unwrap();
    let malformed = synchronize(&tree.root, &mut store, 5_000);
    assert_eq!(malformed.counts.files_failed, 1);
    assert_eq!(malformed.counts.files_imported, 0);
    assert_eq!(malformed.warnings.len(), 1);
    let state = &store.source_states().unwrap()[0];
    assert_eq!(state.last_imported_revision, last_good_revision);
    assert_ne!(state.last_observed_revision, last_good_revision.unwrap());

    fs::remove_file(path).unwrap();
    let missing = synchronize(&tree.root, &mut store, 6_000);
    assert_eq!(missing.counts.files_discovered, 0);
    let state = &store.source_states().unwrap()[0];
    assert!(!state.present);
    assert!(state.last_imported_revision.is_some());
}

#[test]
fn incomplete_final_lines_are_committed_and_retried_without_a_revision_change() {
    let tree = TempTree::new();
    let path = tree.root.join("active.jsonl");
    let source = format!(
        "{}{}{{\"type\":\"message\"",
        header("active-session", None),
        assistant_event("complete-event", 10)
    );
    fs::write(path, source).unwrap();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    let first = synchronize(&tree.root, &mut store, 1_000);
    assert_eq!(first.counts.files_imported, 1);
    assert_eq!(first.counts.incomplete_files_imported, 1);
    assert_eq!(first.counts.observations_inserted, 1);

    let retried = synchronize(&tree.root, &mut store, 2_000);
    assert_eq!(retried.counts.files_unchanged, 0);
    assert_eq!(retried.counts.files_imported, 1);
    assert_eq!(retried.counts.incomplete_files_imported, 1);
    assert_eq!(retried.counts.observations_inserted, 0);
}

#[test]
fn changed_files_import_when_the_scan_timestamp_is_reused() {
    let tree = TempTree::new();
    let path = tree.root.join("same-timestamp.jsonl");
    fs::write(
        &path,
        session("same-timestamp-session", None, &[("event-a", 10)]),
    )
    .unwrap();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    let first = synchronize(&tree.root, &mut store, 1_000);
    assert_eq!(first.counts.files_imported, 1);

    OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(assistant_event("event-b", 20).as_bytes())
        .unwrap();
    let second = synchronize(&tree.root, &mut store, 1_000);

    assert_eq!(second.counts.files_imported, 1);
    assert_eq!(second.counts.files_failed, 0);
    assert_eq!(second.counts.observations_inserted, 1);
}

#[test]
fn a_bad_source_does_not_prevent_another_source_from_importing() {
    let tree = TempTree::new();
    fs::write(
        tree.root.join("good.jsonl"),
        session("good-session", None, &[("good-event", 10)]),
    )
    .unwrap();
    fs::write(
        tree.root.join("bad.jsonl"),
        format!("{}{{bad}}\n", header("bad-session", None)),
    )
    .unwrap();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    let report = synchronize(&tree.root, &mut store, 1_000);

    assert_eq!(report.counts.files_discovered, 2);
    assert_eq!(report.counts.files_imported, 1);
    assert_eq!(report.counts.files_failed, 1);
    assert_eq!(report.counts.event_identities_inserted, 1);
    assert_eq!(report.warnings.len(), 1);
}

#[test]
fn copied_history_has_one_identity_and_an_observation_per_source() {
    let tree = TempTree::new();
    let original = tree.root.join("original.jsonl");
    let clone = tree.root.join("clone.jsonl");
    fs::write(
        &original,
        session("original-session", None, &[("shared-event", 10)]),
    )
    .unwrap();
    fs::write(
        &clone,
        session("clone-session", Some(&original), &[("shared-event", 99)]),
    )
    .unwrap();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    let report = synchronize(&tree.root, &mut store, 1_000);

    assert_eq!(report.counts.files_imported, 2);
    assert_eq!(report.counts.event_identities_inserted, 1);
    assert_eq!(report.counts.observations_inserted, 2);
}

struct MutatingParser {
    path: PathBuf,
    mutate_once: AtomicBool,
}

impl SessionParser for MutatingParser {
    type Error = PiParseError;

    fn parse(&self, input: &mut dyn BufRead) -> Result<ParsedSession, Self::Error> {
        let parsed = PiSessionParser::new().parse(input);
        if self.mutate_once.swap(false, Ordering::SeqCst) {
            OpenOptions::new()
                .append(true)
                .open(&self.path)
                .unwrap()
                .write_all(assistant_event("event-added-during-read", 20).as_bytes())
                .unwrap();
        }
        parsed
    }
}

#[test]
fn a_file_change_during_an_import_attempt_is_retried() {
    let tree = TempTree::new();
    let path = tree.root.join("changing.jsonl");
    fs::write(
        &path,
        session("changing-session", None, &[("initial-event", 10)]),
    )
    .unwrap();
    let parser = MutatingParser {
        path: path.clone(),
        mutate_once: AtomicBool::new(true),
    };
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    let report = synchronize_sessions_at(
        &PiSessionDiscovery::new(&tree.root),
        &parser,
        &mut store,
        scan_time(1_000),
    )
    .unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.counts.files_imported, 1);
    assert_eq!(report.counts.event_identities_inserted, 2);
    assert_eq!(report.counts.observations_inserted, 2);
    assert_eq!(
        store.source_states().unwrap()[0]
            .last_imported_revision
            .as_ref()
            .unwrap()
            .size,
        fs::metadata(path).unwrap().len()
    );
}

struct AlwaysMutatingParser {
    path: PathBuf,
}

impl SessionParser for AlwaysMutatingParser {
    type Error = PiParseError;

    fn parse(&self, input: &mut dyn BufRead) -> Result<ParsedSession, Self::Error> {
        let parsed = PiSessionParser::new().parse(input);
        OpenOptions::new()
            .append(true)
            .open(&self.path)
            .unwrap()
            .write_all(b"{\"type\":\"future-entry\"}\n")
            .unwrap();
        parsed
    }
}

#[test]
fn a_file_that_keeps_changing_is_deferred() {
    let tree = TempTree::new();
    let path = tree.root.join("never-stable.jsonl");
    fs::write(
        &path,
        session("changing-session", None, &[("initial-event", 10)]),
    )
    .unwrap();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();

    let report = synchronize_sessions_at(
        &PiSessionDiscovery::new(&tree.root),
        &AlwaysMutatingParser { path },
        &mut store,
        scan_time(1_000),
    )
    .unwrap();

    assert_eq!(report.counts.files_imported, 0);
    assert_eq!(report.counts.files_failed, 1);
    assert!(report.warnings[0].message.contains("import deferred"));
    assert!(
        store.source_states().unwrap()[0]
            .last_imported_revision
            .is_none()
    );
}
