use std::{fs, sync::Barrier};

use rusqlite::Connection;
use token_tracker::application::{CostAmount, CostTotal};
use token_tracker::domain::TokenCounts;
use token_tracker::domain::export::ExportEstimate;
use token_tracker::{LocalSourceConfig, TokenTracker, TokenTrackerConfig};

use crate::support::{TempTree, fixture};

fn config(tree: &TempTree) -> TokenTrackerConfig {
    TokenTrackerConfig {
        database_path: Some(tree.root.join("usage.db")),
        sources: vec![],
        ..Default::default()
    }
}

#[test]
fn export_matches_reports_and_keeps_shared_sessions_without_refreshing() {
    let tree = TempTree::new();
    for name in ["parent", "fork"] {
        tree.write(
            format!("codex/rollout-{name}.jsonl"),
            fixture("codex", &format!("legacy-{name}.jsonl")),
        );
    }
    let pi = tree.write("pi/session.jsonl", fixture("pi", "all-usage.jsonl"));
    let mut config = config(&tree);
    config.sources = vec![
        LocalSourceConfig::Pi {
            root: Some(tree.root.join("pi")),
        },
        LocalSourceConfig::Codex {
            roots: Some(vec![tree.root.join("codex")]),
        },
    ];
    let mut tracker = TokenTracker::open(config).unwrap();
    assert_eq!(tracker.refresh().unwrap().counts.sources_imported, 3);
    let totals = tracker.report().unwrap().report.totals;
    fs::write(pi, super::adapters::pi_session(999)).unwrap();
    let export = tracker.export_snapshot().unwrap();
    let mut tokens = TokenCounts::default();
    let mut cost = 0.0;
    let mut unavailable = 0;
    for event in &export.events {
        tokens = tokens.checked_add(event.tokens).unwrap();
        let amount = match &event.estimate {
            ExportEstimate::NotNeeded => event.recorded_cost_usd.as_ref().unwrap(),
            ExportEstimate::Available { cost_usd, .. } => cost_usd,
            ExportEstimate::Unavailable { .. } => {
                unavailable += 1;
                continue;
            }
        };
        cost += amount.as_str().parse::<f64>().unwrap();
    }
    assert_eq!(tokens, totals.tokens);
    assert_eq!(export.events.len() as u64, totals.unique_usage_event_count);
    assert_eq!(unavailable, 1);
    assert_eq!(
        unavailable,
        totals.estimates.estimate_candidate_event_count - totals.estimates.priced_event_count
    );
    let CostTotal::Available {
        amount: CostAmount::Usd(expected),
        partial: true,
    } = totals.cost
    else {
        panic!("expected a partial cost total");
    };
    assert!((cost - expected).abs() < 1e-12);
    assert!(export.events.iter().any(|event| event.sessions.len() == 2));
    let session = &export
        .events
        .iter()
        .find(|event| event.agent == "pi")
        .unwrap()
        .sessions[0];
    assert_eq!(session.name.as_deref(), Some("Fixture session"));
    assert_eq!(session.working_directory.as_deref(), Some("/work/project"));
}

#[test]
fn identity_and_revisions_survive_reopening_failed_exports_and_rebuilds() {
    let tree = TempTree::new();
    let mut config = config(&tree);
    config.machine_state_path = Some(tree.root.join("persistent.db"));
    config.machine_name = Some("Workstation".into());
    let first = TokenTracker::open(config.clone())
        .unwrap()
        .export_snapshot()
        .unwrap();
    assert_eq!(first.export_revision, 1);
    assert_eq!(first.machine_name, config.machine_name);
    for revision in [2, 3] {
        let tracker = TokenTracker::open(config.clone()).unwrap();
        let export = tracker.export_snapshot().unwrap();
        assert_eq!(export.machine_id, first.machine_id);
        assert_eq!(export.export_revision, revision);
        if revision == 2 {
            Connection::open(config.database_path.as_ref().unwrap())
                .unwrap()
                .execute_batch("DROP TABLE usage_observations")
                .unwrap();
            assert!(tracker.export_snapshot().is_err());
            drop(tracker);
            fs::remove_file(config.database_path.as_ref().unwrap()).unwrap();
        }
    }
}

#[test]
fn concurrent_exports_share_identity_and_allocate_distinct_revisions() {
    let tree = TempTree::new();
    let first = TokenTracker::open(config(&tree)).unwrap();
    let second = TokenTracker::open(config(&tree)).unwrap();
    let barrier = Barrier::new(2);
    let (first, second) = std::thread::scope(|scope| {
        let barrier = &barrier;
        let worker = scope.spawn(move || {
            barrier.wait();
            first.export_snapshot().unwrap()
        });
        barrier.wait();
        (second.export_snapshot().unwrap(), worker.join().unwrap())
    });
    assert_eq!(first.machine_id, second.machine_id);
    let mut revisions = [first.export_revision, second.export_revision];
    revisions.sort();
    assert_eq!(revisions, [1, 2]);
}
