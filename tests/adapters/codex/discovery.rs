use crate::support::TempTree;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use token_tracker::adapters::files::SessionFileDiscovery;

use token_tracker::adapters::codex::CodexSessionDiscovery;

#[test]
fn discovers_only_rollouts_in_nested_active_and_archive_roots_once_in_path_order() {
    let tree = TempTree::new();
    let active = tree.root.join("sessions");
    let archive = tree.root.join("archived_sessions");
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
    fs::write(tree.root.join("rollout-outside.jsonl"), b"outside roots").unwrap();
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
    let first = tree.root.join("a");
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
