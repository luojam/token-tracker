use crate::support::TempTree;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use token_tracker::adapters::claude::ClaudeSessionDiscovery;
use token_tracker::application::SessionDiscovery;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const CHILD_ONLY_SESSION: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn discovers_main_and_nested_children_only_in_supported_layouts() {
    let tree = TempTree::new();
    let root = tree.root.join("projects");
    let main = tree.write(
        format!("projects/-absent-workspace/{SESSION}.jsonl"),
        b"session",
    );
    let child = tree.write(
        format!("projects/-absent-workspace/{SESSION}/subagents/agent-a1b2c3d.jsonl"),
        b"child",
    );
    let nested = tree.write(format!(
        "projects/-another-workspace/{CHILD_ONLY_SESSION}/subagents/team/deep/agent-b2c3d4e.jsonl"
    ), b"nested child");
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
        tree.write(artifact, b"ignored");
    }
    let discovery = ClaudeSessionDiscovery::new(&root);
    let report = discovery.discover().unwrap();
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
