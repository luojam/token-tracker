use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use token_tracker::adapters::codex::CodexSessionDiscovery;
use token_tracker::adapters::sqlite::SqliteUsageStore;
use token_tracker::application::{SessionDiscovery, UsageStore};
use token_tracker::core::Timestamp;

static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "token-tracker-codex-discovery-{}-{}",
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

#[test]
fn discovers_only_rollouts_in_nested_active_and_archive_roots_once_in_path_order() {
    let tree = TempTree::new();
    let active = tree.0.join("sessions");
    let archive = tree.0.join("archived_sessions");
    let nested = active.join("2026/09/06");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&archive).unwrap();
    let first = archive.join("rollout-archived.jsonl");
    let second = nested.join("rollout-active.jsonl");
    fs::write(&second, b"not JSON; discovery reads only metadata").unwrap();
    fs::write(&first, b"archived").unwrap();
    fs::set_permissions(&second, fs::Permissions::from_mode(0o000)).unwrap();
    for ignored in [
        "history.jsonl",
        "session.jsonl",
        "rollout-backup.jsonl.bak",
        "rollout-other.txt",
    ] {
        fs::write(nested.join(ignored), b"ignored").unwrap();
    }
    fs::write(tree.0.join("rollout-outside.jsonl"), b"outside roots").unwrap();
    let discovery = CodexSessionDiscovery::new([&active, &nested, &archive, &active]);
    let report = discovery.discover().unwrap();
    assert_eq!(discovery.agent_id().as_str(), "codex");
    assert!(report.warnings.is_empty());
    assert!(report.coverage.inaccessible_paths.is_empty());
    assert_eq!(
        report.coverage.inspected_roots,
        vec![archive.clone(), active.clone()]
    );
    assert_eq!(
        report
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    for file in &report.files {
        let metadata = fs::metadata(&file.path).unwrap();
        assert_eq!(file.revision.size, metadata.len());
        assert_eq!(file.revision.modified_at, metadata.modified().unwrap());
    }
    assert_eq!(
        report,
        CodexSessionDiscovery::new([archive, active])
            .discover()
            .unwrap()
    );
}

#[test]
fn roots_with_parent_components_are_not_pruned_as_descendants() {
    let tree = TempTree::new();
    let first = tree.0.join("a");
    let second = first.join("../b");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let rollout = second.join("rollout-session.jsonl");
    fs::write(&rollout, b"usage").unwrap();

    let alone = CodexSessionDiscovery::new([&second]).discover().unwrap();
    let combined = CodexSessionDiscovery::new([&first, &second])
        .discover()
        .unwrap();
    assert_eq!(alone.files.len(), 1);
    assert_eq!(combined.files, alone.files);
    assert_eq!(combined.coverage.inspected_roots, vec![first, second]);
    assert!(combined.warnings.is_empty());
}

#[test]
fn missing_relative_roots_are_inspected_without_warnings() {
    let root = PathBuf::from(format!("missing-codex-root-{}", std::process::id()));
    let absolute = std::env::current_dir().unwrap().join(&root);
    assert!(!absolute.exists());
    let report = CodexSessionDiscovery::new([root]).discover().unwrap();
    assert!(report.files.is_empty());
    assert!(report.warnings.is_empty());
    assert!(report.coverage.inaccessible_paths.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![absolute]);
}

#[test]
fn directory_symlinks_are_excluded_from_scanning_and_absence_coverage() {
    let tree = TempTree::new();
    let root = tree.0.join("sessions");
    let target = tree.0.join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("rollout-hidden.jsonl"), b"outside").unwrap();
    let linked = root.join("linked");
    let named_like_file = root.join("rollout-directory.jsonl");
    let cycle = root.join("cycle");
    symlink(&target, &linked).unwrap();
    symlink(&target, &named_like_file).unwrap();
    symlink(&root, &cycle).unwrap();
    let report = CodexSessionDiscovery::new([&linked, &root])
        .discover()
        .unwrap();
    assert!(report.files.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![root]);
    assert_eq!(
        report.coverage.inaccessible_paths,
        vec![cycle, linked.clone(), named_like_file]
    );
    for suffix in ["", "/", "///"] {
        let mut spelling = linked.clone().into_os_string();
        spelling.push(suffix);
        let report = CodexSessionDiscovery::new([PathBuf::from(spelling)])
            .discover()
            .unwrap();
        assert!(report.files.is_empty());
        assert!(report.coverage.inspected_roots.is_empty());
        assert_eq!(report.coverage.inaccessible_paths, vec![linked.clone()]);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].path, Some(linked.clone()));
    }
}

#[test]
fn failed_inspection_preserves_presence_while_missing_roots_clear_it() {
    let tree = TempTree::new();
    let active = tree.0.join("sessions");
    let blocked = active.join("blocked");
    let archive = tree.0.join("archived_sessions");
    fs::create_dir_all(&blocked).unwrap();
    fs::create_dir(&archive).unwrap();
    let candidate = active.join("rollout-candidate.jsonl");
    let nested = blocked.join("rollout-nested.jsonl");
    let archived = archive.join("rollout-archived.jsonl");
    for path in [&candidate, &nested, &archived] {
        fs::write(path, b"usage").unwrap();
    }
    let discovery = CodexSessionDiscovery::new([&active, &archive]);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let agent = discovery.agent_id();
    store
        .record_discovery(
            &agent,
            &discovery.discover().unwrap(),
            Timestamp::from_unix_milliseconds(1),
        )
        .unwrap();

    fs::remove_file(&candidate).unwrap();
    symlink(tree.0.join("missing-target"), &candidate).unwrap();
    // A file where a directory used to be reliably causes read_dir to fail,
    // even when tests run with elevated filesystem privileges.
    fs::remove_dir_all(&blocked).unwrap();
    fs::write(&blocked, b"not a directory").unwrap();
    let blocked_report = CodexSessionDiscovery::new([&blocked]).discover().unwrap();
    assert!(blocked_report.coverage.inspected_roots.is_empty());
    assert_eq!(
        blocked_report.coverage.inaccessible_paths,
        vec![blocked.clone()]
    );
    store
        .record_discovery(
            &agent,
            &blocked_report,
            Timestamp::from_unix_milliseconds(2),
        )
        .unwrap();
    assert!(
        store
            .source_states(&agent)
            .unwrap()
            .iter()
            .find(|source| source.path == nested)
            .unwrap()
            .present
    );

    fs::remove_dir_all(&archive).unwrap();
    let report = discovery.discover().unwrap();
    assert_eq!(report.files.len(), 0);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].path, Some(candidate.clone()));
    assert_eq!(report.coverage.inaccessible_paths, vec![candidate.clone()]);
    store
        .record_discovery(&agent, &report, Timestamp::from_unix_milliseconds(3))
        .unwrap();
    let states = store.source_states(&agent).unwrap();
    assert!(
        states
            .iter()
            .find(|source| source.path == candidate)
            .unwrap()
            .present
    );
    assert!(
        !states
            .iter()
            .find(|source| source.path == archived)
            .unwrap()
            .present
    );
    assert!(
        !states
            .iter()
            .find(|source| source.path == nested)
            .unwrap()
            .present
    );
}

#[test]
fn unreadable_subtree_is_reported_and_excluded_from_coverage() {
    let tree = TempTree::new();
    let blocked = tree.0.join("blocked");
    fs::create_dir(&blocked).unwrap();
    let candidate = blocked.join("rollout-hidden.jsonl");
    fs::write(&candidate, b"usage").unwrap();
    let discovery = CodexSessionDiscovery::new([&tree.0]);
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    store
        .record_discovery(
            &discovery.agent_id(),
            &discovery.discover().unwrap(),
            Timestamp::from_unix_milliseconds(1),
        )
        .unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let report = discovery.discover().unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    // Privileged test runners can inspect mode-000 directories.
    if report.files.iter().any(|file| file.path == candidate) {
        return;
    }
    assert!(report.files.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![tree.0.clone()]);
    assert_eq!(report.coverage.inaccessible_paths, vec![blocked.clone()]);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].path, Some(blocked));
    store
        .record_discovery(
            &discovery.agent_id(),
            &report,
            Timestamp::from_unix_milliseconds(2),
        )
        .unwrap();
    assert!(store.source_states(&discovery.agent_id()).unwrap()[0].present);
}
