use crate::support::{TempTree, fixture};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs,
    io::Write,
};
use token_tracker::{
    LocalSourceConfig, TokenTracker, TokenTrackerConfig,
    application::{
        CostTotal, UsageReadStore, build_usage_report, calculate_usage_summary_filtered,
    },
    domain::{ReportFilters, ReportingPeriod},
    remote::{
        ObjectMetadata, RemoteError, RemoteSnapshot, RemoteStore, SnapshotCache, build_snapshot,
        combine_snapshots,
    },
    storage::SqliteUsageStore,
};

#[derive(Default)]
struct MemoryRemote {
    objects: RefCell<BTreeMap<String, (String, Vec<u8>)>>,
    downloads: Cell<usize>,
    revision: Cell<u64>,
    fail_list: Cell<bool>,
    fail_download: Cell<bool>,
    conflict: Cell<bool>,
    listed_etag: RefCell<Option<String>>,
}

impl RemoteStore for MemoryRemote {
    fn list(&self) -> token_tracker::remote::Result<Vec<ObjectMetadata>> {
        if self.fail_list.get() {
            return Err(RemoteError::Invalid("offline".into()));
        }

        Ok(self
            .objects
            .borrow()
            .iter()
            .map(|(id, (etag, body))| ObjectMetadata {
                key: format!("machines/{id}.json"),
                etag: self.listed_etag.borrow().as_ref().unwrap_or(etag).clone(),
                size: body.len() as u64,
                uploaded: "2026-01-01T00:00:00Z".into(),
            })
            .collect())
    }

    fn download(&self, key: &str, output: &mut dyn Write) -> token_tracker::remote::Result<String> {
        self.downloads.set(self.downloads.get() + 1);
        if self.fail_download.get() {
            output.write_all(b"{partial")?;
            return Err(RemoteError::Invalid("interrupted".into()));
        }

        let objects = self.objects.borrow();
        let (etag, bytes) = objects.get(&key[9..41]).unwrap();
        output.write_all(bytes)?;

        Ok(etag.clone())
    }

    fn upload(
        &self,
        id: &str,
        previous: Option<&str>,
        body: Vec<u8>,
    ) -> token_tracker::remote::Result<()> {
        let mut objects = self.objects.borrow_mut();
        if self.conflict.get() || objects.get(id).map(|(etag, _)| etag.as_str()) != previous {
            return Err(RemoteError::Conflict);
        }

        self.revision.set(self.revision.get() + 1);
        objects.insert(id.into(), (format!("\"{}\"", self.revision.get()), body));

        Ok(())
    }
}

fn tracker(tree: &TempTree) -> TokenTracker {
    TokenTracker::open(TokenTrackerConfig {
        database_path: Some(tree.root.join("usage.db")),
        machine_name: Some("Laptop".into()),
        sources: vec![
            LocalSourceConfig::Pi {
                root: Some(tree.root.join("pi")),
            },
            LocalSourceConfig::Claude {
                root: Some(tree.root.join("claude")),
            },
        ],
        ..Default::default()
    })
    .unwrap()
}

fn sources(tree: &TempTree) {
    tree.write("pi/session.jsonl", fixture("pi", "all-usage.jsonl"));
    tree.write(
        "claude/project/11111111-1111-4111-8111-111111111111.jsonl",
        fixture("claude", "snapshots.jsonl"),
    );
}

#[test]
fn daily_snapshots_preserve_local_filtered_tokens_costs_and_pricing_completeness() {
    let tree = TempTree::new();
    sources(&tree);
    let mut tracker = tracker(&tree);
    tracker.refresh().unwrap();

    let stored = SqliteUsageStore::open_read_only(tree.root.join("usage.db"))
        .unwrap()
        .usage_snapshot()
        .unwrap();
    let snapshot = build_snapshot(&stored, "a".repeat(32), None, 1).unwrap();
    let serialized = serde_json::to_value(&snapshot).unwrap();

    let keys = |value: &serde_json::Value| {
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };

    assert_eq!(
        keys(&serialized),
        [
            "format_version",
            "generated_at_unix_ms",
            "machine_id",
            "machine_name",
            "rows"
        ]
    );
    for row in serialized["rows"].as_array().unwrap() {
        assert_eq!(
            keys(row),
            [
                "agent",
                "costs",
                "date",
                "model",
                "provider",
                "tokens",
                "usage_kind"
            ]
        );
        assert_eq!(
            keys(&row["costs"]),
            [
                "estimate_candidates",
                "estimate_overflow",
                "estimated_usd",
                "priced_events",
                "rate_snapshots",
                "recorded_usd"
            ]
        );
    }

    let snapshot: RemoteSnapshot = serde_json::from_value(serialized).unwrap();
    let now = "2026-01-01T12:00:00Z".parse().unwrap();

    for period in [
        ReportingPeriod::AllTime,
        ReportingPeriod::Day,
        ReportingPeriod::Week,
        ReportingPeriod::Month,
    ] {
        for filters in [
            ReportFilters::default(),
            ReportFilters {
                agents: vec!["pi".into(), "claude".into()],
                providers: vec!["anthropic".into()],
                models: vec![],
            },
            ReportFilters {
                agents: vec!["PI".into()],
                ..Default::default()
            },
            ReportFilters {
                models: vec!["claude-opus-5".into(), "missing".into()],
                ..Default::default()
            },
        ] {
            let local = build_usage_report(
                &calculate_usage_summary_filtered(&stored, period.range_at(now), &filters).unwrap(),
            );
            let remote = combine_snapshots(
                std::slice::from_ref(&snapshot),
                period.range_at(now),
                &filters,
            )
            .unwrap();

            assert_eq!(remote.totals.tokens, local.totals.tokens);
            assert_eq!(remote.totals.cost, local.totals.cost);
            assert_eq!(
                remote.totals.estimates.priced_event_count,
                local.totals.estimates.priced_event_count
            );
            assert_eq!(
                remote.totals.estimates.estimate_candidate_event_count,
                local.totals.estimates.estimate_candidate_event_count
            );
        }
    }

    let all = combine_snapshots(
        std::slice::from_ref(&snapshot),
        None,
        &ReportFilters::default(),
    )
    .unwrap();
    assert!(matches!(
        all.totals.cost,
        CostTotal::Available { partial: true, .. }
    ));

    let mut other = snapshot.clone();
    other.machine_id = "b".repeat(32);
    let combined =
        combine_snapshots(&[snapshot.clone(), other], None, &ReportFilters::default()).unwrap();
    assert_eq!(combined.totals.tokens.input, all.totals.tokens.input * 2);

    assert!(
        combine_snapshots(
            &[snapshot.clone(), snapshot],
            None,
            &ReportFilters::default()
        )
        .is_err()
    );
}

#[test]
fn sync_replaces_old_usage_reuses_identity_and_rebuilds_after_conflict() {
    let tree = TempTree::new();
    sources(&tree);
    let mut tracker = tracker(&tree);
    let remote = MemoryRemote::default();

    let (first, _) = tracker.sync_remote(&remote).unwrap();
    assert_eq!(
        first.machine_id,
        tracker.export_snapshot().unwrap().machine_id
    );

    let lock = fs::OpenOptions::new()
        .write(true)
        .open(tree.root.join("machine-state.sync.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    assert!(tracker.sync_remote(&remote).is_err());
    fs2::FileExt::unlock(&lock).unwrap();

    let source = tree.root.join("pi/session.jsonl");
    let corrected = fixture("pi", "all-usage.jsonl").replace("\"input\":10", "\"input\":1");
    assert_ne!(corrected, fs::read_to_string(&source).unwrap());
    fs::write(&source, corrected).unwrap();

    remote.conflict.set(true);
    assert!(matches!(
        tracker.sync_remote(&remote),
        Err(RemoteError::Conflict)
    ));

    let retained: RemoteSnapshot =
        serde_json::from_slice(&remote.objects.borrow()[&first.machine_id].1).unwrap();
    assert_eq!(retained, first);

    remote.conflict.set(false);
    let (next, _) = tracker.sync_remote(&remote).unwrap();
    assert_eq!(remote.objects.borrow().len(), 1);
    assert_ne!(next.rows, first.rows);
    assert_eq!(
        combine_snapshots(&[next], None, &ReportFilters::default())
            .unwrap()
            .totals
            .tokens,
        tracker.report().unwrap().report.totals.tokens
    );
}

#[test]
fn cache_uses_download_etag_and_preserves_valid_files_on_refresh_failure() {
    let tree = TempTree::new();
    sources(&tree);
    let mut tracker = tracker(&tree);
    let remote = MemoryRemote::default();
    let (first, _) = tracker.sync_remote(&remote).unwrap();
    let cache = SnapshotCache::new(&tree.root.join("cache"), "https://one.example/");

    *remote.listed_etag.borrow_mut() = Some("outdated-listing".into());
    assert_eq!(
        cache.refresh(&remote).unwrap(),
        std::slice::from_ref(&first)
    );

    *remote.listed_etag.borrow_mut() = None;
    assert_eq!(
        cache.refresh(&remote).unwrap(),
        std::slice::from_ref(&first)
    );
    assert_eq!(remote.downloads.get(), 1);

    remote.fail_list.set(true);
    assert!(cache.refresh(&remote).is_err());
    remote.fail_list.set(false);

    *remote.listed_etag.borrow_mut() = Some("changed".into());
    remote.fail_download.set(true);
    assert!(cache.refresh(&remote).is_err());
    remote.fail_download.set(false);

    let saved = remote.objects.borrow()[&first.machine_id].clone();
    remote
        .objects
        .borrow_mut()
        .get_mut(&first.machine_id)
        .unwrap()
        .1 = b"{}".to_vec();
    assert!(cache.refresh(&remote).is_err());

    remote
        .objects
        .borrow_mut()
        .insert(first.machine_id.clone(), saved);
    *remote.listed_etag.borrow_mut() = None;
    assert_eq!(cache.refresh(&remote).unwrap(), [first]);
    assert_eq!(remote.downloads.get(), 3);

    let other = SnapshotCache::new(&tree.root.join("cache"), "https://two.example/");
    other.refresh(&remote).unwrap();
    assert_eq!(remote.downloads.get(), 4);

    remote.objects.borrow_mut().clear();
    assert!(cache.refresh(&remote).unwrap().is_empty());
}

#[test]
fn cache_rolls_back_partial_refreshes_without_removing_committed_snapshots() {
    let tree = TempTree::new();
    let remote = MemoryRemote::default();
    let first = RemoteSnapshot {
        format_version: 1,
        machine_id: "a".repeat(32),
        machine_name: None,
        generated_at_unix_ms: 0,
        rows: Vec::new(),
    };
    let second = RemoteSnapshot {
        machine_id: "b".repeat(32),
        ..first.clone()
    };
    let originals = BTreeMap::from([
        (
            first.machine_id.clone(),
            ("a1".into(), serde_json::to_vec(&first).unwrap()),
        ),
        (
            second.machine_id.clone(),
            ("b1".into(), serde_json::to_vec(&second).unwrap()),
        ),
    ]);
    *remote.objects.borrow_mut() = originals.clone();
    let root = tree.root.join("cache");
    let cache = SnapshotCache::new(&root, "https://one.example/");
    let expected = vec![first.clone(), second.clone()];
    assert_eq!(cache.refresh(&remote).unwrap(), expected);

    let directory = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
    let files = || -> BTreeMap<_, _> {
        fs::read_dir(&directory)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect()
    };
    let committed = files();

    for etag in ["a1", "a2", "a3"] {
        *remote.listed_etag.borrow_mut() = Some("changed".into());
        remote.objects.borrow_mut().insert(
            first.machine_id.clone(),
            (etag.into(), serde_json::to_vec(&first).unwrap()),
        );
        remote
            .objects
            .borrow_mut()
            .insert(second.machine_id.clone(), ("b2".into(), b"{}".to_vec()));

        let downloads = remote.downloads.get();
        assert!(cache.refresh(&remote).is_err());
        assert_eq!(remote.downloads.get(), downloads + 2);
        assert_eq!(files(), committed);

        *remote.objects.borrow_mut() = originals.clone();
        *remote.listed_etag.borrow_mut() = None;
        assert_eq!(cache.refresh(&remote).unwrap(), expected);
        assert_eq!(remote.downloads.get(), downloads + 2);
    }

    remote.objects.borrow_mut().insert(
        first.machine_id.clone(),
        ("a4".into(), serde_json::to_vec(&first).unwrap()),
    );
    assert_eq!(cache.refresh(&remote).unwrap(), expected);
    assert_eq!(files().len(), committed.len());
    let downloads = remote.downloads.get();
    assert_eq!(cache.refresh(&remote).unwrap(), expected);
    assert_eq!(remote.downloads.get(), downloads);
}

#[test]
fn snapshot_money_round_trips_at_picodollar_precision_and_rejects_invalid_values() {
    use token_tracker::{
        application::CostAmount,
        domain::{EstimatedCost, TokenCounts},
        remote::{SnapshotCosts, SnapshotRow},
    };

    let mut snapshot = RemoteSnapshot {
        format_version: 1,
        machine_id: "a".repeat(32),
        machine_name: None,
        generated_at_unix_ms: 0,
        rows: vec![SnapshotRow {
            date: "2026-01-01".into(),
            agent: "codex".into(),
            provider: Some("openai".into()),
            model: Some("model".into()),
            usage_kind: None,
            tokens: TokenCounts::default(),
            costs: SnapshotCosts {
                recorded_usd: None,
                estimated_usd: None,
                estimate_overflow: false,
                estimate_candidates: 2,
                priced_events: 1,
                rate_snapshots: BTreeMap::from([("rates-v1".into(), "2026-01-01".into())]),
            },
        }],
    };

    for amount in [1, u128::MAX] {
        let cost = EstimatedCost::from_picodollars(amount);
        snapshot.rows[0].costs.estimated_usd = Some(cost.into());

        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let decoded = serde_json::from_slice(&encoded).unwrap();
        let report = combine_snapshots(&[decoded], None, &ReportFilters::default()).unwrap();

        assert_eq!(
            report.totals.cost,
            CostTotal::Available {
                amount: CostAmount::Estimated(cost),
                partial: true
            }
        );
    }

    let mut other = snapshot.clone();
    other.machine_id = "b".repeat(32);
    let report =
        combine_snapshots(&[snapshot.clone(), other], None, &ReportFilters::default()).unwrap();
    assert_eq!(report.totals.cost, CostTotal::Overflow);

    snapshot.rows[0].costs.priced_events = 3;
    assert!(snapshot.validate().is_err());

    snapshot.rows[0].costs.priced_events = 1;
    snapshot.rows[0].costs.estimated_usd = Some("0.0000000000001".to_owned().try_into().unwrap());
    assert!(snapshot.validate().is_err());
}
