use crate::support::{TempTree, fixture, prefix};
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;
use token_tracker::adapters::claude::{ClaudeSessionDiscovery, ClaudeSessionParser};
use token_tracker::application::{
    SynchronizationReport, UsageReadStore, UsageSnapshot, summarize_usage, synchronize_sessions_at,
};
use token_tracker::domain::{
    CacheWriteTokens, KnownRequests, ParentSession, PricingContext, RequestBreakdown, ServiceSpeed,
    Timestamp, UsageEvent,
};
use token_tracker::storage::SqliteUsageStore;

const MAIN: &str = "11111111-1111-4111-8111-111111111111";

struct Ledger {
    tree: TempTree,
    clock: u64,
}

impl Ledger {
    fn new() -> Self {
        Self {
            tree: TempTree::new(),
            clock: 0,
        }
    }

    fn projects(&self) -> PathBuf {
        self.tree.root.join("projects")
    }

    fn store(&self) -> SqliteUsageStore {
        SqliteUsageStore::open(self.tree.root.join("usage.db")).unwrap()
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
        self.write(fixture_path(name), &fixture("claude", name))
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
}

fn oracle() -> Value {
    serde_json::from_str(include_str!("../fixtures/claude/expectations.json")).unwrap()
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(oracle()["fixtures"][name]["source_path"].as_str().unwrap())
        .strip_prefix("/invented/claude/projects")
        .unwrap()
        .to_owned()
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
fn final_responses_and_billing_corrections_survive_reopen() {
    let mut ledger = Ledger::new();
    let source = fixture("claude", "snapshots.jsonl");
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
        &format!("{}\n{correction}\n{placeholder}\n", prefix(&source, 6)),
    );
    let report = ledger.sync();
    assert!(report.warnings.is_empty());
    assert_eq!(report.counts.observations_updated, 1);
    expected.tokens.input = 8;
    expected.tokens.cache_read = 90;
    expected.tokens.output = 25;
    let Some(PricingContext::Anthropic(facts)) = expected.pricing_context.as_mut() else {
        panic!("expected Anthropic billing");
    };
    facts.speed = ServiceSpeed::Fast;
    facts.requests = RequestBreakdown::KnownRequests(KnownRequests::new(expected.tokens));
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
fn shared_history_and_children_are_counted_once() {
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

        let child = fixture("claude", "child-b2c3d4e.jsonl");
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
fn placeholders_do_not_suppress_complete_copies() {
    let mut ledger = Ledger::new();
    let main_path = fixture_path("snapshots.jsonl");
    ledger.write(
        &main_path,
        &prefix(&fixture("claude", "snapshots.jsonl"), 3),
    );
    ledger.write_fixture("shared-history.jsonl");
    assert_eq!(ledger.sync().counts.files_failed, 0);
    totals(&ledger, 2, 2, 168);
    ledger.write(
        &main_path,
        &prefix(&fixture("claude", "snapshots.jsonl"), 4),
    );
    assert!(ledger.sync().warnings.is_empty());
    totals(&ledger, 2, 2, 175);
}
