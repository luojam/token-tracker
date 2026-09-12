use std::fs;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use token_tracker::adapters::pi::{PiSessionDiscovery, PiSessionParser};
use token_tracker::application::{
    DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, FileRevision, ParseCompletion,
    ParseContext, ParsedSession, SessionAdapter, SessionDiscovery, SessionParser, UsageReadStore,
    UsageStore, build_usage_report, summarize_usage, synchronize_sessions_at,
};
use token_tracker::domain::{
    AgentId, ParentSession, SessionMetadata, Timestamp, TokenCounts, UsageEvent,
    UsageEventIdentity, UsageKind,
};
use token_tracker::storage::SqliteUsageStore;

static NEXT_TREE: AtomicU64 = AtomicU64::new(0);
struct TempTree(PathBuf);
impl TempTree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "token-tracker-adapters-{}-{}",
            std::process::id(),
            NEXT_TREE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

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

// Session IDs come from filenames; contents are "parent-id-or-dash input-tokens".
struct TestParser {
    agent: &'static str,
}
impl SessionParser for TestParser {
    type Error = io::Error;
    fn parse(
        &self,
        input: &mut dyn BufRead,
        context: ParseContext<'_>,
    ) -> Result<ParsedSession, Self::Error> {
        let mut line = String::new();
        input.read_line(&mut line)?;
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 {
            return Err(io::Error::other("invalid test usage"));
        }
        let session_id = context
            .source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| io::Error::other("invalid test session filename"))?;
        let parent = (fields[0] != "-").then(|| ParentSession::SessionId(fields[0].into()));
        Ok(ParsedSession {
            metadata: SessionMetadata {
                agent: self.agent.into(),
                session_id: session_id.into(),

                working_directory: None,
                // Ancestry must outrank the earlier child timestamp.
                started_at: Timestamp::from_unix_milliseconds(if parent.is_some() {
                    1000
                } else {
                    2000
                }),
                name: None,
                parent_session: parent,
            },
            events: vec![UsageEvent {
                identity: UsageEventIdentity {
                    agent: self.agent.into(),
                    // Matches the Pi key to test agent isolation.
                    adapter_key: "v1:assistant:1000:shared".into(),
                },
                timestamp: Timestamp::from_unix_milliseconds(1000),
                kind: UsageKind::Other,
                attribution: None,
                tokens: TokenCounts {
                    input: fields[1].parse().map_err(io::Error::other)?,
                    ..TokenCounts::default()
                },
                recorded_cost: None,
                pricing_context: None,
            }],
            completion: ParseCompletion::Complete,
            notices: Vec::new(),
        })
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
fn a_second_adapter_preserves_identity_lineage_and_failure_isolation() {
    let tree = TempTree::new();
    let pi_path = tree.0.join("pi.jsonl");
    let original = tree.0.join("original.usage");
    let child = tree.0.join("child.usage");
    fs::write(&pi_path, pi_session(7)).unwrap();
    fs::write(&original, "- 10\n").unwrap();
    fs::write(&child, "original 999\n").unwrap();
    let pi = SessionAdapter::new(PiSessionDiscovery::new(&tree.0), PiSessionParser::new());
    let test_adapter = |files, fail| {
        SessionAdapter::new(
            TestDiscovery {
                agent: "test-agent",
                root: tree.0.clone(),
                files,
                fail,
            },
            TestParser {
                agent: "test-agent",
            },
        )
    };
    let test = test_adapter(vec![original.clone(), child.clone()], false);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let report = run_all_time_report(&[&pi, &test], &mut store, vec![]).unwrap();
    assert!(report.contains("Input tokens: 17\n"), "{report}");
    assert!(report.contains("Sessions: 3\n"));
    assert!(report.contains("Unique usage events: 2\n"));
    assert!(report.lines().any(|line| {
        line.split_whitespace().collect::<Vec<_>>().join(" ")
            == "Unattributed other usage 10 0 0 0 10 1 unavailable"
    }));
    assert!(store.source_states(&"pi".into()).unwrap()[0].present);
    assert_eq!(store.source_states(&"test-agent".into()).unwrap().len(), 2);
    assert_eq!(
        run_all_time_report(&[&test, &pi], &mut store, vec![]).unwrap(),
        report
    );

    fs::remove_file(&original).unwrap();
    let remaining = test_adapter(vec![child], false);
    assert_eq!(
        run_all_time_report(&[&remaining, &pi], &mut store, vec![]).unwrap(),
        report
    );
    assert!(
        store
            .source_states(&"test-agent".into())
            .unwrap()
            .iter()
            .any(|source| source.path == original && !source.present)
    );

    fs::write(&pi_path, pi_session(88)).unwrap();
    let unavailable = test_adapter(vec![], true);
    let report = run_all_time_report(&[&unavailable, &pi], &mut store, vec![]).unwrap();
    assert!(report.contains("Input tokens: 98\n"), "{report}");
    assert!(report.contains("test-agent: session discovery failed: discovery unavailable"));
}

#[test]
fn identical_paths_have_independent_revisions_presence_and_scan_times() {
    let tree = TempTree::new();
    let path = tree.0.join("shared.usage");
    fs::write(&path, "- 10\n").unwrap();
    let discovery = |agent, files| TestDiscovery {
        agent,
        root: tree.0.clone(),
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

    fs::write(&path, "- 200\n").unwrap();
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

fn run_all_time_report<S: UsageStore + UsageReadStore>(
    adapters: &[&dyn token_tracker::application::ImportAdapter<S>],
    store: &mut S,
    warnings: Vec<token_tracker::application::ImportWarning>,
) -> Result<String, token_tracker::application::AllTimeReportError> {
    let report = token_tracker::application::run_all_time_report(adapters, store, warnings)?;
    Ok(token_tracker::cli::render_terminal_report(
        &build_usage_report(&report.summary),
        &report.warnings,
    ))
}
