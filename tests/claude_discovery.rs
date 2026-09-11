use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use token_tracker::adapters::claude::ClaudeSessionDiscovery;
use token_tracker::application::{SessionDiscovery, UsageStore};
use token_tracker::domain::Timestamp;
use token_tracker::storage::SqliteUsageStore;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const CHILD_ONLY_SESSION: &str = "22222222-2222-4222-8222-222222222222";
static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

struct TempTree(PathBuf);

impl TempTree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "token-tracker-claude-discovery-{}-{}",
            std::process::id(),
            NEXT_TREE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn write(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"discovery needs no transcript contents").unwrap();
        path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn discovers_main_and_nested_children_only_in_supported_layouts() {
    let tree = TempTree::new();
    let root = tree.0.join("projects");
    let main = tree.write(format!("projects/-absent-workspace/{SESSION}.jsonl"));
    let child = tree.write(format!(
        "projects/-absent-workspace/{SESSION}/subagents/agent-a1b2c3d.jsonl"
    ));
    let nested = tree.write(format!(
        "projects/-another-workspace/{CHILD_ONLY_SESSION}/subagents/team/deep/agent-b2c3d4e.jsonl"
    ));
    fs::set_permissions(&main, fs::Permissions::from_mode(0o000)).unwrap();
    for artifact in [
        format!("projects/{SESSION}.jsonl"),
        "projects/-absent-workspace/history.jsonl".into(),
        "projects/-absent-workspace/sessions-index.json".into(),
        "projects/-absent-workspace/agent-legacy.jsonl".into(),
        format!("projects/-absent-workspace/{SESSION}.jsonl.bak"),
        format!("projects/-absent-workspace/memory/{SESSION}.jsonl"),
        format!("projects/-absent-workspace/snapshots/{SESSION}.jsonl"),
        format!("projects/-absent-workspace/{SESSION}/snapshots/agent-hidden.jsonl"),
        format!("projects/-absent-workspace/{SESSION}/agent-wrong-layout.jsonl"),
        format!("projects/-absent-workspace/{SESSION}/subagents/metadata.jsonl"),
        format!("projects/-absent-workspace/{SESSION}/subagents/agent-.jsonl"),
        format!("projects/-absent-workspace/{SESSION}/subagents/agent-invalid:id.jsonl"),
        format!("projects/-absent-workspace/{SESSION}/subagents/agent-a.meta.json"),
        format!("projects/-absent-workspace/{SESSION}/subagents/agent-a.jsonl.bak"),
        format!("outside/-project/{SESSION}.jsonl"),
    ] {
        tree.write(artifact);
    }
    let discovery = ClaudeSessionDiscovery::new(&root);
    let report = discovery.discover().unwrap();
    assert_eq!(discovery.agent_id().as_str(), "claude");
    assert_eq!(discovery.root(), root);
    assert!(report.warnings.is_empty());
    assert!(report.coverage.inaccessible_paths.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![root]);
    let mut expected = [main, child, nested.clone()];
    expected.sort();
    assert_eq!(
        report
            .files
            .iter()
            .map(|file| &file.path)
            .collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    for file in &report.files {
        assert!(file.path.is_absolute());
        let metadata = fs::metadata(&file.path).unwrap();
        assert_eq!(file.revision.size, metadata.len());
        assert_eq!(file.revision.modified_at, metadata.modified().unwrap());
    }
    assert_eq!(report, discovery.discover().unwrap());
    fs::write(&nested, b"updated child").unwrap();
    let changed = discovery.discover().unwrap();
    for (before, after) in report.files.iter().zip(&changed.files) {
        if before.path == nested {
            assert_ne!(before.revision, after.revision);
        } else {
            assert_eq!(before, after);
        }
    }
}

#[test]
fn missing_relative_root_has_complete_empty_coverage() {
    let root = PathBuf::from(format!("missing-claude-root-{}", std::process::id()));
    let absolute = std::env::current_dir().unwrap().join(&root);
    assert!(!absolute.exists());
    let report = ClaudeSessionDiscovery::new(root).discover().unwrap();
    assert!(report.files.is_empty());
    assert!(report.warnings.is_empty());
    assert!(report.coverage.inaccessible_paths.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![absolute]);
}

#[test]
fn symlinks_to_directories_and_broken_sources_preserve_presence() {
    let tree = TempTree::new();
    let root = tree.0.join("projects");
    let main = tree.write(format!("projects/-project/{SESSION}.jsonl"));
    let child = tree.write(format!(
        "projects/-project/{SESSION}/subagents/agent-a.jsonl"
    ));
    let discovery = ClaudeSessionDiscovery::new(&root);
    let agent = discovery.agent_id();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    store
        .record_discovery(
            &agent,
            &discovery.discover().unwrap(),
            Timestamp::from_unix_milliseconds(1),
        )
        .unwrap();

    let subagents = child.parent().unwrap();
    let target = tree.0.join("outside");
    fs::rename(subagents, &target).unwrap();
    symlink(&target, subagents).unwrap();
    fs::remove_file(&main).unwrap();
    symlink(tree.0.join("missing-target"), &main).unwrap();
    let cycle = root.join("cycle");
    symlink(&root, &cycle).unwrap();
    let report = discovery.discover().unwrap();
    assert!(report.files.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![root.clone()]);
    let mut inaccessible = vec![main.clone(), subagents.to_owned(), cycle];
    inaccessible.sort();
    assert_eq!(report.coverage.inaccessible_paths, inaccessible);
    assert_eq!(report.warnings.len(), 3);
    assert_eq!(report, discovery.discover().unwrap());
    store
        .record_discovery(&agent, &report, Timestamp::from_unix_milliseconds(2))
        .unwrap();
    assert!(
        store
            .source_states(&agent)
            .unwrap()
            .iter()
            .all(|s| s.present)
    );

    for suffix in ["", "/", "///"] {
        let mut spelling = subagents.as_os_str().to_owned();
        spelling.push(suffix);
        let report = ClaudeSessionDiscovery::new(PathBuf::from(spelling))
            .discover()
            .unwrap();
        assert!(report.files.is_empty());
        assert!(report.coverage.inspected_roots.is_empty());
        assert_eq!(
            report.coverage.inaccessible_paths,
            vec![subagents.to_owned()]
        );
    }

    fs::remove_dir_all(&root).unwrap();
    store
        .record_discovery(
            &agent,
            &discovery.discover().unwrap(),
            Timestamp::from_unix_milliseconds(3),
        )
        .unwrap();
    assert!(
        store
            .source_states(&agent)
            .unwrap()
            .iter()
            .all(|s| !s.present)
    );
}

#[test]
fn unreadable_subagent_directory_does_not_mark_sources_missing() {
    let tree = TempTree::new();
    let root = tree.0.join("projects");
    let child = tree.write(format!(
        "projects/-project/{SESSION}/subagents/nested/agent-a.jsonl"
    ));
    let blocked = child.parent().unwrap();
    let discovery = ClaudeSessionDiscovery::new(&root);
    let agent = discovery.agent_id();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    store
        .record_discovery(
            &agent,
            &discovery.discover().unwrap(),
            Timestamp::from_unix_milliseconds(1),
        )
        .unwrap();
    fs::set_permissions(blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let report = discovery.discover().unwrap();
    fs::set_permissions(blocked, fs::Permissions::from_mode(0o700)).unwrap();
    // Privileged runners can still read mode-000 directories.
    if report.files.iter().any(|file| file.path == child) {
        return;
    }
    assert!(report.files.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![root]);
    assert_eq!(report.coverage.inaccessible_paths, vec![blocked.to_owned()]);
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].path.as_deref(), Some(blocked));
    store
        .record_discovery(&agent, &report, Timestamp::from_unix_milliseconds(2))
        .unwrap();
    assert!(store.source_states(&agent).unwrap()[0].present);
}

#[test]
fn uninspectable_root_does_not_establish_absence() {
    let tree = TempTree::new();
    let root = tree.0.join("projects");
    tree.write(format!("projects/-project/{SESSION}.jsonl"));
    let discovery = ClaudeSessionDiscovery::new(&root);
    let agent = discovery.agent_id();
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    store
        .record_discovery(
            &agent,
            &discovery.discover().unwrap(),
            Timestamp::from_unix_milliseconds(1),
        )
        .unwrap();
    fs::remove_dir_all(&root).unwrap();
    fs::write(&root, b"not a directory").unwrap();
    let report = discovery.discover().unwrap();
    assert!(report.files.is_empty());
    assert!(report.coverage.inspected_roots.is_empty());
    assert_eq!(report.coverage.inaccessible_paths, vec![root]);
    assert_eq!(report.warnings.len(), 1);
    store
        .record_discovery(&agent, &report, Timestamp::from_unix_milliseconds(2))
        .unwrap();
    assert!(store.source_states(&agent).unwrap()[0].present);
}
