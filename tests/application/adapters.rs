use crate::support::TempTree;
use std::fs;
use std::io::{self, BufRead};
use std::path::PathBuf;
use token_tracker::adapters::files::{
    FileDiscoveryReport, FileSessionSource, ParseContext, SessionFileDiscovery, SessionParser,
};

use token_tracker::adapters::pi::{PiSessionDiscovery, PiSessionParser};
use token_tracker::application::{
    SessionData, UsageReadStore, UsageStore, run_all_time_report, summarize_usage,
    synchronize_sessions_at,
};
use token_tracker::domain::{AgentId, Timestamp};
use token_tracker::storage::SqliteUsageStore;

struct TestDiscovery {
    agent: &'static str,
    root: PathBuf,
    files: Vec<PathBuf>,
    fail: bool,
}
impl SessionFileDiscovery for TestDiscovery {
    type Error = io::Error;
    fn agent_id(&self) -> AgentId {
        self.agent.into()
    }
    fn discover(&self) -> Result<FileDiscoveryReport, Self::Error> {
        if self.fail {
            return Err(io::Error::other("discovery unavailable"));
        }
        let mut report = PiSessionDiscovery::new(&self.root)
            .discover()
            .map_err(io::Error::other)?;
        report.files.retain(|file| self.files.contains(&file.path));
        Ok(report)
    }
}

pub(super) struct TestParser {
    pub(super) agent: &'static str,
}

impl SessionParser for TestParser {
    type Error = token_tracker::adapters::pi::PiParseError;

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<SessionData, Self::Error> {
        let mut parsed = PiSessionParser::new().parse(input, context)?;
        parsed.metadata.agent = self.agent.into();
        for event in &mut parsed.events {
            event.identity.agent = self.agent.into();
        }
        Ok(parsed)
    }
}

pub(super) fn pi_session(tokens: u64) -> String {
    format!(
        r#"{{"type":"session","version":3,"id":"original","timestamp":"1970-01-01T00:00:02Z","cwd":"/work"}}
{{"type":"message","id":"shared","timestamp":"1970-01-01T00:00:01Z","message":{{"role":"assistant","provider":"provider","model":"model","usage":{{"input":{tokens},"output":0,"cacheRead":0,"cacheWrite":0}}}}}}
"#
    )
}

#[test]
fn invalid_imports_warn_without_preventing_reporting() {
    let tree = TempTree::new();
    let valid = pi_session(7);
    tree.write("valid.jsonl", &valid);
    tree.write("oversized.jsonl", pi_session(u64::MAX));
    let conflicting = pi_session(20);
    tree.write(
        "conflicting.jsonl",
        format!("{valid}{}\n", conflicting.lines().nth(1).unwrap()),
    );
    let pi = FileSessionSource::new(PiSessionDiscovery::new(&tree.root), PiSessionParser::new());
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let report = run_all_time_report(&[&pi], &mut store, vec![]).unwrap();
    assert_eq!(report.summary.totals.tokens.input, 7);
    assert_eq!(report.warnings.len(), 2);
    assert!(report.warnings.iter().any(|warning| {
        warning.message == "pi: conflicting usage events with the same identity"
    }));
    assert!(report.warnings.iter().any(|warning| {
        warning.message == "pi: token count exceeds the supported integer range"
    }));
}

#[test]
fn discovery_failure_does_not_prevent_other_adapters_from_importing() {
    let tree = TempTree::new();
    tree.write("pi.jsonl", pi_session(7));
    let pi = FileSessionSource::new(PiSessionDiscovery::new(&tree.root), PiSessionParser::new());
    let unavailable = FileSessionSource::new(
        TestDiscovery {
            agent: "test-agent",
            root: tree.root.clone(),
            files: vec![],
            fail: true,
        },
        TestParser {
            agent: "test-agent",
        },
    );
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let report = run_all_time_report(&[&unavailable, &pi], &mut store, vec![]).unwrap();
    assert_eq!(report.summary.totals.tokens.input, 7);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(
        report.warnings[0].message,
        "test-agent: session discovery failed: discovery unavailable"
    );
}

#[test]
fn agents_have_independent_usage_revisions_and_presence_at_the_same_path() {
    let tree = TempTree::new();
    let path = tree.root.join("shared.jsonl");
    fs::write(&path, pi_session(10)).unwrap();
    let discovery = |agent, files| TestDiscovery {
        agent,
        root: tree.root.clone(),
        files,
        fail: false,
    };
    let first = discovery("first", vec![path.clone()]);
    let second = discovery("second", vec![path.clone()]);
    let sync = |store: &mut SqliteUsageStore, source: &TestDiscovery, time| {
        synchronize_sessions_at(
            &FileSessionSource::new(
                source,
                TestParser {
                    agent: source.agent,
                },
            ),
            store,
            Timestamp::from_unix_milliseconds(time),
        )
        .unwrap()
        .counts
    };
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    for source in [&first, &second] {
        assert_eq!(sync(&mut store, source, 1).sources_imported, 1);
    }
    assert_eq!(sync(&mut store, &second, 2).sources_unchanged, 1);
    sync(&mut store, &discovery("first", vec![]), 100);
    assert!(!store.source_states(&"first".into()).unwrap()[0].present);
    assert!(store.source_states(&"second".into()).unwrap()[0].present);

    fs::write(&path, pi_session(200)).unwrap();
    assert_eq!(sync(&mut store, &second, 3).sources_imported, 1);
    let summary = summarize_usage(&store.usage_snapshot().unwrap()).unwrap();
    assert_eq!(summary.totals.tokens.input, 210);
    assert_eq!(summary.totals.unique_usage_event_count, 2);

    let rejected = synchronize_sessions_at(
        &FileSessionSource::new(
            &discovery("third", vec![path]),
            &TestParser { agent: "second" },
        ),
        &mut store,
        Timestamp::from_unix_milliseconds(4),
    )
    .unwrap();
    assert_eq!(rejected.counts.sources_failed, 1);
    assert!(
        store.source_states(&"third".into()).unwrap()[0]
            .last_import
            .is_none()
    );
    assert_eq!(
        summarize_usage(&store.usage_snapshot().unwrap()).unwrap(),
        summary
    );
}
