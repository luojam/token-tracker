use std::fs::{self, File, FileTimes};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};
use token_tracker::cli::render_terminal_report;

use serde_json::Value;
use token_tracker::adapters::claude::{ClaudeSessionDiscovery, ClaudeSessionParser};
use token_tracker::application::{
    SourceState, SynchronizationReport, UsageReadStore, UsageSnapshot, UsageStore, summarize_usage,
    synchronize_sessions_at,
};
use token_tracker::domain::{
    AgentId, CacheWriteTokens, ParentSession, ServiceTier, Timestamp, UsageEvent,
};
use token_tracker::storage::SqliteUsageStore;

const MAIN: &str = "11111111-1111-4111-8111-111111111111";
const COPY: &str = "22222222-2222-4222-8222-222222222222";
static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

struct Ledger {
    root: PathBuf,
    clock: u64,
}

impl Ledger {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "token-tracker-claude-lifecycle-{}-{}",
            std::process::id(),
            NEXT_TREE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root, clock: 0 }
    }

    fn projects(&self) -> PathBuf {
        self.root.join("projects")
    }

    fn store(&self) -> SqliteUsageStore {
        SqliteUsageStore::open(self.root.join("usage.db")).unwrap()
    }

    fn write(&mut self, relative: impl AsRef<Path>, source: &str) -> PathBuf {
        let path = self.projects().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, source).unwrap();
        self.clock += 1;
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(self.clock)))
            .unwrap();
        path
    }

    fn write_fixture(&mut self, name: &str) -> PathBuf {
        self.write(fixture_path(name), &fixture(name))
    }

    fn sync(&mut self) -> SynchronizationReport {
        self.clock += 1;
        synchronize_sessions_at(
            &ClaudeSessionDiscovery::new(self.projects()),
            &ClaudeSessionParser::new(),
            &mut self.store(),
            Timestamp::from_unix_milliseconds(self.clock as i64),
        )
        .unwrap()
    }

    fn snapshot(&self) -> UsageSnapshot {
        self.store().usage_snapshot().unwrap()
    }

    fn states(&self) -> Vec<SourceState> {
        self.store()
            .source_states(&AgentId::from("claude"))
            .unwrap()
    }
}

impl Drop for Ledger {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn oracle() -> Value {
    serde_json::from_str(include_str!("fixtures/claude/expectations.json")).unwrap()
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(oracle()["fixtures"][name]["source_path"].as_str().unwrap())
        .strip_prefix("/invented/claude/projects")
        .unwrap()
        .to_owned()
}

fn fixture(name: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude")
            .join(name),
    )
    .unwrap()
}

fn prefix(source: &str, lines: usize) -> String {
    source.split_inclusive('\n').take(lines).collect()
}

fn event(snapshot: &UsageSnapshot, key: &str) -> UsageEvent {
    let events: Vec<_> = snapshot
        .observations
        .iter()
        .filter(|observation| observation.event.identity.adapter_key == key)
        .collect();
    assert_eq!(events.len(), 1);
    events[0].event.clone()
}

fn totals(ledger: &Ledger, sessions: u64, events: u64, tokens: u128) {
    let summary = summarize_usage(&ledger.snapshot()).unwrap();
    assert_eq!(summary.totals.session_count, sessions);
    assert_eq!(summary.totals.unique_usage_event_count, events);
    assert_eq!(summary.totals.tokens.total(), tokens);
}

#[test]
fn reopened_imports_append_finals_and_correct_tokens_and_pricing_without_duplicates() {
    let mut ledger = Ledger::new();
    let source = fixture("snapshots.jsonl");
    let path = fixture_path("snapshots.jsonl");
    ledger.write(&path, &prefix(&source, 3));
    let partial = ledger.sync();
    assert_eq!(partial.warnings.len(), 1);
    totals(&ledger, 1, 0, 0);
    let unchanged = ledger.sync();
    assert_eq!(unchanged.counts.files_unchanged, 1);
    assert_eq!(unchanged.warnings, partial.warnings);

    ledger.write(&path, &prefix(&source, 6));
    assert!(ledger.sync().warnings.is_empty());
    totals(&ledger, 1, 2, 340);
    let mut expected = event(&ledger.snapshot(), "response-v1:msg_shared");
    let equal = event(&ledger.snapshot(), "response-v1:msg_equal");

    let mut correction: Value = serde_json::from_str(source.lines().nth(6).unwrap()).unwrap();
    let usage = &mut correction["message"]["usage"];
    usage["speed"] = "fast".into();
    usage["cache_creation"]["ephemeral_5m_input_tokens"] = 20.into();
    usage["cache_creation"]["ephemeral_1h_input_tokens"] = 20.into();
    let placeholder = source.lines().nth(7).unwrap();
    ledger.write(
        &path,
        &format!("{}{correction}\n{placeholder}\n", prefix(&source, 6)),
    );
    let report = ledger.sync();
    assert!(report.warnings.is_empty());
    assert_eq!(report.counts.observations_updated, 1);
    expected.tokens.input = 8;
    expected.tokens.cache_read = 90;
    expected.tokens.output = 25;
    let facts = expected.pricing_context.as_mut().unwrap();
    facts.speed = ServiceTier::Fast;
    facts.request_usage = Some(vec![expected.tokens]);
    facts.cache_writes = Some(vec![
        CacheWriteTokens {
            duration_seconds: 300,
            tokens: 20,
        },
        CacheWriteTokens {
            duration_seconds: 3600,
            tokens: 20,
        },
    ]);
    assert_eq!(
        event(&ledger.snapshot(), "response-v1:msg_shared"),
        expected
    );
    assert_eq!(event(&ledger.snapshot(), "response-v1:msg_equal"), equal);
    totals(&ledger, 1, 2, 333);
    let snapshot = ledger.snapshot();
    assert_eq!(ledger.sync().counts.files_unchanged, 1);
    assert_eq!(ledger.snapshot(), snapshot);
}

#[test]
fn shared_history_and_children_match_the_oracle_across_import_orders() {
    let names = [
        "snapshots.jsonl",
        "shared-history.jsonl",
        "child-a1b2c3d.jsonl",
        "child-b2c3d4e.jsonl",
    ];
    let mut expected_summary = None;
    for order in [[0, 1, 2, 3], [3, 2, 1, 0]] {
        let mut ledger = Ledger::new();
        for (index, fixture_index) in order.into_iter().enumerate() {
            ledger.write_fixture(names[fixture_index]);
            assert!(ledger.sync().warnings.is_empty());
            if index == 0 && fixture_index == 3 {
                totals(&ledger, 1, 1, 5);
                let child = &ledger.snapshot().sessions[0];
                assert_eq!(child.key.session_id, format!("subagent-v1:{MAIN}:b2c3d4e"));
                assert_eq!(
                    child.parent_session,
                    Some(ParentSession::SessionId(MAIN.into()))
                );
            }
        }
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.observations.len(), 6);
        totals(&ledger, 4, 5, 348);
        let summary = summarize_usage(&snapshot).unwrap();
        assert_eq!(&summary, expected_summary.get_or_insert(summary.clone()));
        assert_eq!(ledger.sync().counts.files_unchanged, 4);
        assert_eq!(ledger.snapshot(), snapshot);

        let child = fixture("child-b2c3d4e.jsonl");
        let mut correction: Value = serde_json::from_str(child.trim()).unwrap();
        correction["message"]["usage"]["output_tokens"] = 13.into();
        ledger.write(
            fixture_path("child-b2c3d4e.jsonl"),
            &format!("{child}{correction}\n"),
        );
        let changed = ledger.sync();
        assert!(changed.warnings.is_empty());
        assert_eq!(changed.counts.files_imported, 1);
        assert_eq!(changed.counts.files_unchanged, 3);
        assert_eq!(changed.counts.observations_updated, 1);
        totals(&ledger, 4, 5, 358);
    }
}

#[test]
fn placeholders_cannot_suppress_complete_copies_and_conflicts_use_stable_precedence() {
    for main_lines in [3, 4] {
        for order in [[0, 1], [1, 0]] {
            let mut ledger = Ledger::new();
            let main_path = fixture_path("snapshots.jsonl");
            let copy_path = fixture_path("shared-history.jsonl");
            let main = prefix(&fixture("snapshots.jsonl"), main_lines);
            let copy = fixture("shared-history.jsonl");
            let sources = [(&main_path, &main), (&copy_path, &copy)];
            for index in order {
                ledger.write(sources[index].0, sources[index].1);
                assert_eq!(ledger.sync().counts.files_failed, 0);
            }
            totals(&ledger, 2, 2, if main_lines == 3 { 168 } else { 175 });
            ledger.write(&main_path, &prefix(&fixture("snapshots.jsonl"), 4));
            assert!(ledger.sync().warnings.is_empty());
            totals(&ledger, 2, 2, 175);

            let mut earlier: Value = serde_json::from_str(copy.lines().next().unwrap()).unwrap();
            earlier["timestamp"] = "2025-12-31T23:59:59Z".into();
            ledger.write(&copy_path, &format!("{earlier}\n{copy}"));
            assert!(ledger.sync().warnings.is_empty());
            totals(&ledger, 2, 2, 168);

            let alternate = copy.replace("\"input_tokens\":8", "\"input_tokens\":18");
            let path = ledger.write(
                format!("aaa/{COPY}.jsonl"),
                &format!("{earlier}\n{alternate}"),
            );
            assert!(ledger.sync().warnings.is_empty());
            totals(&ledger, 2, 2, 178);
            fs::remove_file(path).unwrap();
            assert!(ledger.sync().warnings.is_empty());
            totals(&ledger, 2, 2, 178);
        }
    }
}

#[test]
fn failed_inaccessible_deleted_and_rewritten_sources_retain_usage_and_notices() {
    let mut ledger = Ledger::new();
    ledger.write_fixture("partial-ignored.jsonl");
    let initial = ledger.sync();
    assert_eq!(initial.warnings.len(), 1);
    let snapshot = ledger.snapshot();
    let state = ledger.states().remove(0);
    assert_eq!(state.notices[0].count.get(), 2);
    totals(&ledger, 1, 1, 5);
    let unchanged = ledger.sync();
    assert_eq!(unchanged.counts.files_unchanged, 1);
    assert_eq!(unchanged.warnings, initial.warnings);
    let summary = summarize_usage(&snapshot).unwrap();
    let estimate = &summary.estimates[&AgentId::from("claude")];
    assert_eq!(estimate.totals.imported_event_count, 1);
    assert_eq!(estimate.totals.priced_event_count, 1);
    let report = render_terminal_report(&summary, &unchanged.warnings);
    assert!(report.contains("Claude Code usage:"));
    assert!(report.contains("Total cost: $0.000085\n"));
    assert!(!report.contains("(partial)"));
    assert!(report.contains("Warnings (1):"));
    assert!(report.contains(&initial.warnings[0].message));

    ledger.write_fixture("reject-malformed.jsonl");
    let failed = ledger.sync();
    assert_eq!(failed.counts.files_failed, 1);
    assert!(failed.warnings.contains(&initial.warnings[0]));
    let failed_state = ledger.states().remove(0);
    assert_eq!(
        failed_state.last_imported_revision,
        state.last_imported_revision
    );
    assert_eq!(failed_state.notices, state.notices);
    assert_eq!(ledger.snapshot(), snapshot);

    let moved = ledger.root.join("uninspected-projects");
    fs::rename(ledger.projects(), &moved).unwrap();
    symlink(&moved, ledger.projects()).unwrap();
    let inaccessible = ledger.sync();
    assert_eq!(inaccessible.counts.files_discovered, 0);
    assert_eq!(inaccessible.warnings.len(), 2);
    assert!(inaccessible.warnings.contains(&initial.warnings[0]));
    assert!(ledger.states()[0].present);
    assert_eq!(ledger.snapshot(), snapshot);

    fs::remove_file(ledger.projects()).unwrap();
    assert_eq!(ledger.sync().warnings, initial.warnings);
    assert!(!ledger.states()[0].present);
    assert_eq!(ledger.snapshot(), snapshot);

    ledger.write_fixture("metadata-only.jsonl");
    assert!(ledger.sync().warnings.is_empty());
    assert!(ledger.states()[0].notices.is_empty());
    assert!(ledger.states()[0].present);
    assert_eq!(ledger.snapshot().observations, snapshot.observations);
    totals(&ledger, 1, 1, 5);
}
