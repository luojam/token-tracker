use crate::support::TempTree;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use token_tracker::adapters::files::{
    FileDiscoveryReport, FileSessionSource, SessionFileDiscovery,
};
use token_tracker::adapters::pi::{PiSessionDiscovery, PiSessionParser};
use token_tracker::application::{SessionSource, UsageStore};

use token_tracker::domain::{AgentId, Timestamp};
use token_tracker::storage::SqliteUsageStore;

fn scan(root: &Path) -> FileDiscoveryReport {
    PiSessionDiscovery::new(root).discover().unwrap()
}

#[test]
fn skipped_links_preserve_presence_while_readable_files_are_discovered() {
    let tree = TempTree::new();
    let root = tree.root.join("sessions");
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("session.jsonl"), b"session").unwrap();
    let agent = AgentId::from("pi");
    let mut store = SqliteUsageStore::open_in_memory().unwrap();
    let source = FileSessionSource::new(PiSessionDiscovery::new(&root), PiSessionParser::new());
    let record = |store: &mut SqliteUsageStore, time| {
        let report = source
            .discover(&store.source_states(&agent).unwrap())
            .unwrap();
        store
            .record_discovery(&agent, &report, Timestamp::from_unix_milliseconds(time))
            .unwrap();
    };
    record(&mut store, 1);

    let target = tree.root.join("outside");
    fs::rename(&project, &target).unwrap();
    symlink(&target, &project).unwrap();
    symlink(&target, root.join("directory.jsonl")).unwrap();
    symlink(&root, root.join("cycle")).unwrap();
    for name in ["broken", "broken.jsonl"] {
        symlink(tree.root.join("missing"), root.join(name)).unwrap();
    }
    let valid = root.join("valid.jsonl");
    symlink(target.join("session.jsonl"), &valid).unwrap();

    let report = scan(&root);
    assert_eq!(report.files.len(), 1);
    assert_eq!(report.files[0].path, valid);
    assert_eq!(report.files[0].revision.size, 7);
    assert_eq!(report.coverage.inspected_roots, vec![root.clone()]);
    assert_eq!(
        report.coverage.inaccessible_paths,
        [
            "broken",
            "broken.jsonl",
            "cycle",
            "directory.jsonl",
            "project"
        ]
        .map(|name| root.join(name))
    );
    assert_eq!(report.warnings.len(), 5);
    assert_eq!(report, scan(&root));
    record(&mut store, 2);
    let states = store.source_states(&agent).unwrap();
    assert_eq!(states.len(), 2);
    assert!(states.iter().all(|source| source.present));

    fs::remove_dir_all(&root).unwrap();
    record(&mut store, 3);
    assert!(
        store
            .source_states(&agent)
            .unwrap()
            .iter()
            .all(|s| !s.present)
    );
}

#[test]
fn symlink_and_unreadable_roots_do_not_establish_absence() {
    let tree = TempTree::new();
    let root = tree.root.join("root");
    symlink(&tree.root, &root).unwrap();
    for suffix in ["", "/", "///"] {
        let mut spelling = root.as_os_str().to_owned();
        spelling.push(suffix);
        let report = scan(Path::new(&spelling));
        assert!(report.files.is_empty());
        assert!(report.coverage.inspected_roots.is_empty());
        assert_eq!(report.coverage.inaccessible_paths, vec![root.clone()]);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].path.as_ref(), Some(&root));
    }

    fs::remove_file(&root).unwrap();
    // A file makes read_dir fail even for privileged runners.
    fs::write(&root, b"not a directory").unwrap();
    let report = scan(&root);
    assert!(report.coverage.inspected_roots.is_empty());
    assert_eq!(report.coverage.inaccessible_paths, vec![root.clone()]);
    assert_eq!(report.warnings.len(), 1);

    fs::remove_file(&root).unwrap();
    let report = scan(&root);
    assert_eq!(report.coverage.inspected_roots, vec![root]);
    assert!(report.coverage.inaccessible_paths.is_empty());
    assert!(report.warnings.is_empty());
}
