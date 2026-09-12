use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use token_tracker::adapters::pi::{PiParseError, PiSessionDiscovery, PiSessionParser};
use token_tracker::application::{
    ParseCompletion, ParseContext, ParseNotice, ParsedSession, SessionParser, UsageReadStore,
    UsageStore, synchronize_sessions_at,
};
use token_tracker::domain::{AgentId, Timestamp};
use token_tracker::storage::SqliteUsageStore;

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

    fs::write(&path, session("session-a", None, &[("event-a", 999_999)])).unwrap();
    let rewritten = synchronize(&tree.root, &mut store, 4_000);
    assert_eq!(rewritten.counts.files_imported, 1);
    assert_eq!(rewritten.counts.event_identities_inserted, 0);
    assert_eq!(rewritten.counts.observations_inserted, 0);
    assert_eq!(rewritten.counts.observations_updated, 1);
    let last_good_import = store.source_states(&AgentId::from("pi")).unwrap()[0]
        .last_import
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
    let state = &store.source_states(&AgentId::from("pi")).unwrap()[0];
    assert_eq!(state.last_import, last_good_import);
    assert_ne!(
        state.last_observed_revision,
        last_good_import.unwrap().revision
    );

    fs::remove_file(path).unwrap();
    let missing = synchronize(&tree.root, &mut store, 6_000);
    assert_eq!(missing.counts.files_discovered, 0);
    let state = &store.source_states(&AgentId::from("pi")).unwrap()[0];
    assert!(!state.present);
    assert!(state.last_import.is_some());
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

struct NoticeParser(Vec<ParseNotice>);

impl SessionParser for NoticeParser {
    type Error = PiParseError;

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error> {
        let mut parsed = PiSessionParser::new().parse(input, context)?;
        parsed.notices = self.0.clone();
        Ok(parsed)
    }
}

#[test]
fn notices_survive_scans_reopen_and_failures_until_a_successful_replacement() {
    let tree = TempTree::new();
    let root = tree.root.join("sessions");
    fs::create_dir(&root).unwrap();
    let path = root.join("session.jsonl");
    let source = session("partial", None, &[("final-response", 10)]);
    fs::write(&path, &source).unwrap();
    let database = tree.root.join("usage.db");
    let mut store = SqliteUsageStore::open(&database).unwrap();
    let notices = vec![
        ParseNotice {
            code: "incomplete_response_usage".into(),
            message: "omitted 2 responses with incomplete usage".into(),
            count: 2.try_into().unwrap(),
            line: None,
        },
        ParseNotice {
            code: "unsupported_response_accounting".into(),
            message: "omitted 3 responses with unsupported accounting".into(),
            count: 3.try_into().unwrap(),
            line: Some(4.try_into().unwrap()),
        },
    ];
    let first = synchronize_sessions_at(
        &PiSessionDiscovery::new(&root),
        &NoticeParser(notices.clone()),
        &mut store,
        scan_time(1_000),
    )
    .unwrap();
    assert_eq!(first.counts.files_imported, 1);
    assert_eq!(first.counts.incomplete_files_imported, 0);
    assert_eq!(first.warnings.len(), 2);
    assert_eq!(first.warnings[0].path.as_ref(), Some(&path));
    assert_eq!(
        first.warnings[0].message,
        "omitted 2 responses with incomplete usage"
    );
    assert_eq!(
        first.warnings[1].message,
        "omitted 3 responses with unsupported accounting (first affected line: 4)"
    );
    let snapshot = store.usage_snapshot().unwrap();

    let unchanged = synchronize(&root, &mut store, 2_000);
    assert_eq!(unchanged.counts.files_unchanged, 1);
    assert_eq!(unchanged.warnings, first.warnings);
    drop(store);
    let mut store = SqliteUsageStore::open(&database).unwrap();
    let reopened = synchronize(&root, &mut store, 3_000);
    assert_eq!(reopened.counts.files_unchanged, 1);
    assert_eq!(reopened.warnings, first.warnings);
    assert_eq!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .unwrap()
            .notices,
        notices
    );

    fs::write(&path, session("partial", None, &[("final-response", 999)])).unwrap();
    let failed = synchronize_sessions_at(
        &PiSessionDiscovery::new(&root),
        &NoticeParser(vec![notices[0].clone(), notices[0].clone()]),
        &mut store,
        scan_time(4_000),
    )
    .unwrap();
    assert_eq!(failed.counts.files_failed, 1);
    assert_eq!(failed.warnings.len(), 3);
    assert!(
        first
            .warnings
            .iter()
            .all(|warning| failed.warnings.contains(warning))
    );
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);
    assert_eq!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .unwrap()
            .notices,
        notices
    );

    let moved = tree.root.join("unavailable");
    fs::rename(&root, &moved).unwrap();
    fs::write(&root, b"temporarily not a directory").unwrap();
    let inaccessible = synchronize(&root, &mut store, 5_000);
    assert_eq!(inaccessible.counts.files_discovered, 0);
    assert!(
        first
            .warnings
            .iter()
            .all(|warning| inaccessible.warnings.contains(warning))
    );
    assert!(store.source_states(&"pi".into()).unwrap()[0].present);
    fs::remove_file(&root).unwrap();
    fs::rename(&moved, &root).unwrap();

    fs::remove_file(&path).unwrap();
    let missing = synchronize(&root, &mut store, 6_000);
    assert_eq!(missing.warnings, first.warnings);
    assert!(!store.source_states(&"pi".into()).unwrap()[0].present);
    assert_eq!(store.usage_snapshot().unwrap(), snapshot);

    fs::write(&path, session("partial", None, &[("final-response", 999)])).unwrap();
    let replaced = synchronize(&root, &mut store, 7_000);
    assert_eq!(replaced.counts.files_imported, 1);
    assert_eq!(replaced.counts.observations_updated, 1);
    assert!(replaced.warnings.is_empty());
    drop(store);
    let mut store = SqliteUsageStore::open(&database).unwrap();
    assert!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .unwrap()
            .notices
            .is_empty()
    );
    let unchanged = synchronize(&root, &mut store, 8_000);
    assert_eq!(unchanged.counts.files_unchanged, 1);
    assert!(unchanged.warnings.is_empty());
}

#[test]
fn truncated_tail_notice_counts_tails_and_still_retries_incomplete_files() {
    let tree = TempTree::new();
    let path = tree.root.join("session.jsonl");
    fs::write(
        &path,
        format!("{}{{\"type\":", session("partial", None, &[("final", 10)])),
    )
    .unwrap();
    let parser = NoticeParser(vec![ParseNotice {
        code: "truncated_tail".into(),
        message: "omitted 1 truncated JSON tail; usage may be missing".into(),
        count: 1.try_into().unwrap(),
        line: Some(3.try_into().unwrap()),
    }]);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    for time in [1_000, 2_000] {
        let report = synchronize_sessions_at(
            &PiSessionDiscovery::new(&tree.root),
            &parser,
            &mut store,
            scan_time(time),
        )
        .unwrap();
        assert_eq!(report.counts.files_imported, 1);
        assert_eq!(report.counts.incomplete_files_imported, 1);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            report.warnings[0].message,
            "omitted 1 truncated JSON tail; usage may be missing (first affected line: 3)"
        );
    }
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

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error> {
        assert_eq!(context.source_path, self.path);
        let parsed = PiSessionParser::new().parse(input, context);
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
        store.source_states(&AgentId::from("pi")).unwrap()[0]
            .last_import
            .as_ref()
            .unwrap()
            .revision
            .size,
        fs::metadata(path).unwrap().len()
    );
}

struct AlwaysMutatingParser {
    path: PathBuf,
}

impl SessionParser for AlwaysMutatingParser {
    type Error = PiParseError;

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error> {
        assert_eq!(context.source_path, self.path);
        let parsed = PiSessionParser::new().parse(input, context);
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
        store.source_states(&AgentId::from("pi")).unwrap()[0]
            .last_import
            .is_none()
    );
}

#[test]
fn normalization_versions_reimport_unchanged_sources_and_retry_failures() {
    struct VersionedParser {
        version: u32,
        fail: bool,
        incomplete: bool,
    }
    impl SessionParser for VersionedParser {
        type Error = std::io::Error;

        fn normalization_version(&self) -> std::num::NonZeroU32 {
            self.version.try_into().unwrap()
        }

        fn parse(
            &self,
            input: &mut dyn BufRead,
            context: ParseContext<'_>,
        ) -> Result<ParsedSession, Self::Error> {
            if self.fail {
                return Err(std::io::Error::other("normalization failed"));
            }
            let mut parsed = PiSessionParser::new()
                .parse(input, context)
                .map_err(std::io::Error::other)?;
            parsed.events[0].tokens.input *= u64::from(self.version);
            if self.version == 4 {
                parsed.events.clear();
            }
            if self.incomplete {
                parsed.completion = ParseCompletion::IncompleteFinalLine;
            }
            Ok(parsed)
        }
    }
    let tree = TempTree::new();
    fs::write(
        tree.root.join("session.jsonl"),
        session("versioned", None, &[("event", 10)]),
    )
    .unwrap();
    let database = tree.root.join("usage.db");
    let discovery = PiSessionDiscovery::new(&tree.root);
    let mut store = SqliteUsageStore::open(&database).unwrap();
    let sync = |store: &mut SqliteUsageStore, version, fail, time| {
        synchronize_sessions_at(
            &discovery,
            &VersionedParser {
                version,
                fail,
                incomplete: false,
            },
            store,
            scan_time(time),
        )
        .unwrap()
    };
    assert_eq!(sync(&mut store, 1, false, 1).counts.files_imported, 1);
    assert_eq!(sync(&mut store, 1, false, 2).counts.files_unchanged, 1);
    assert_eq!(sync(&mut store, 2, true, 3).counts.files_failed, 1);
    assert_eq!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .map(|import| import.normalization_version.get()),
        Some(1)
    );
    assert_eq!(
        store.usage_snapshot().unwrap().observations[0]
            .event
            .tokens
            .input,
        10
    );
    assert_eq!(sync(&mut store, 2, false, 4).counts.observations_updated, 1);
    drop(store);
    let mut store = SqliteUsageStore::open(&database).unwrap();
    assert_eq!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .map(|import| import.normalization_version.get()),
        Some(2)
    );
    assert_eq!(
        store.usage_snapshot().unwrap().observations[0]
            .event
            .tokens
            .input,
        20
    );
    assert_eq!(sync(&mut store, 2, false, 5).counts.files_unchanged, 1);

    let before = store.usage_snapshot().unwrap();
    let incomplete = synchronize_sessions_at(
        &discovery,
        &VersionedParser {
            version: 3,
            fail: false,
            incomplete: true,
        },
        &mut store,
        scan_time(6),
    )
    .unwrap();
    assert_eq!(incomplete.counts.files_failed, 1);
    assert_eq!(store.usage_snapshot().unwrap(), before);
    assert_eq!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .map(|import| import.normalization_version.get()),
        Some(2)
    );

    assert_eq!(sync(&mut store, 3, false, 7).counts.files_imported, 1);
    let normalized = store.usage_snapshot().unwrap();
    assert_eq!(normalized.observations.len(), 1);
    assert_eq!(
        normalized.observations[0].event.identity,
        before.observations[0].event.identity
    );
    assert_eq!(normalized.observations[0].event.tokens.input, 30);
    assert_eq!(normalized.sessions, before.sessions);

    assert_eq!(sync(&mut store, 4, false, 8).counts.files_imported, 1);
    drop(store);
    let mut store = SqliteUsageStore::open(&database).unwrap();
    assert_eq!(store.usage_snapshot().unwrap(), normalized);
    assert_eq!(
        store.source_states(&"pi".into()).unwrap()[0]
            .last_import
            .as_ref()
            .map(|import| import.normalization_version.get()),
        Some(4)
    );
    assert_eq!(sync(&mut store, 4, false, 9).counts.files_unchanged, 1);
}
