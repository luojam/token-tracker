use crate::support::TempTree;
use std::fs;
use std::io::{self, BufRead};
use std::path::PathBuf;

use token_tracker::adapters::pi::{PiSessionDiscovery, PiSessionParser};
use token_tracker::application::{
    DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, FileRevision, ParseContext,
    ParsedSession, SessionAdapter, SessionDiscovery, SessionParser, UsageReadStore, UsageStore,
    run_all_time_report, summarize_usage, synchronize_sessions_at,
};
use token_tracker::domain::{AgentId, Timestamp};
use token_tracker::storage::SqliteUsageStore;

struct TestDiscovery {
    agent: &'static str,
    root: PathBuf,
    files: Vec<PathBuf>,
    fail: bool,
}
impl SessionDiscovery for TestDiscovery {
    type Error = io::Error;
    fn agent_id(&self) -> AgentId {
        self.agent.into()
    }
    fn discover(&self) -> Result<DiscoveryReport, Self::Error> {
        if self.fail {
            return Err(io::Error::other("discovery unavailable"));
        }
        Ok(DiscoveryReport {
            files: self
                .files
                .iter()
                .map(|path| {
                    let metadata = fs::metadata(path)?;
                    Ok(DiscoveredSessionFile {
                        path: path.clone(),
                        revision: FileRevision {
                            size: metadata.len(),
                            modified_at: metadata.modified()?,
                        },
                    })
                })
                .collect::<io::Result<_>>()?,
            warnings: vec![],
            coverage: DiscoveryCoverage {
                inspected_roots: vec![self.root.clone()],
                inaccessible_paths: vec![],
            },
        })
    }
}

struct TestParser {
    agent: &'static str,
}

impl SessionParser for TestParser {
    type Error = token_tracker::adapters::pi::PiParseError;

    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error> {
        let mut parsed = PiSessionParser::new().parse(input, context)?;
        parsed.metadata.agent = self.agent.into();
        for event in &mut parsed.events {
            event.identity.agent = self.agent.into();
        }
        Ok(parsed)
    }
}

fn pi_session(tokens: u64) -> String {
    format!(
        r#"{{"type":"session","version":3,"id":"original","timestamp":"1970-01-01T00:00:02Z","cwd":"/work"}}
{{"type":"message","id":"shared","timestamp":"1970-01-01T00:00:01Z","message":{{"role":"assistant","provider":"provider","model":"model","usage":{{"input":{tokens},"output":0,"cacheRead":0,"cacheWrite":0}}}}}}
"#
    )
}

#[test]
fn discovery_failure_does_not_prevent_other_adapters_from_importing() {
    let tree = TempTree::new();
    tree.write("pi.jsonl", pi_session(7));
    let pi = SessionAdapter::new(PiSessionDiscovery::new(&tree.root), PiSessionParser::new());
    let unavailable = SessionAdapter::new(
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
    let path = tree.root.join("shared.usage");
    fs::write(&path, pi_session(10)).unwrap();
    let discovery = |agent, files| TestDiscovery {
        agent,
        root: tree.root.clone(),
        files,
        fail: false,
    };
    let first = discovery("first", vec![path.clone()]);
    let second = discovery("second", vec![path.clone()]);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    for source in [&first, &second] {
        let result = synchronize_sessions_at(
            source,
            &TestParser {
                agent: source.agent,
            },
            &mut store,
            Timestamp::from_unix_milliseconds(1),
        )
        .unwrap();
        assert_eq!(result.counts.files_imported, 1);
    }
    let result = synchronize_sessions_at(
        &second,
        &TestParser { agent: "second" },
        &mut store,
        Timestamp::from_unix_milliseconds(2),
    )
    .unwrap();
    assert_eq!(result.counts.files_unchanged, 1);
    synchronize_sessions_at(
        &discovery("first", vec![]),
        &TestParser { agent: "first" },
        &mut store,
        Timestamp::from_unix_milliseconds(100),
    )
    .unwrap();
    assert!(!store.source_states(&"first".into()).unwrap()[0].present);
    assert!(store.source_states(&"second".into()).unwrap()[0].present);

    fs::write(&path, pi_session(200)).unwrap();
    let result = synchronize_sessions_at(
        &second,
        &TestParser { agent: "second" },
        &mut store,
        Timestamp::from_unix_milliseconds(3),
    )
    .unwrap();
    assert_eq!(result.counts.files_imported, 1);
    let summary = summarize_usage(&store.usage_snapshot().unwrap()).unwrap();
    assert_eq!(summary.totals.tokens.input, 210);
    assert_eq!(summary.totals.unique_usage_event_count, 2);

    let rejected = synchronize_sessions_at(
        &discovery("third", vec![path]),
        &TestParser { agent: "second" },
        &mut store,
        Timestamp::from_unix_milliseconds(4),
    )
    .unwrap();
    assert_eq!(rejected.counts.files_failed, 1);
    assert_eq!(rejected.counts.files_imported, 0);
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
