use crate::support::TempTree;
use std::env;
use std::fs;
use token_tracker::adapters::files::SessionFileDiscovery;
use token_tracker::adapters::pi::{PiDiscoveryError, PiSessionDiscovery};

#[test]
fn discovery_rejects_an_empty_root() {
    assert!(matches!(
        PiSessionDiscovery::new("").discover(),
        Err(PiDiscoveryError::EmptySessionRoot)
    ));
}

#[test]
fn discovery_resolves_relative_roots_to_absolute_paths() {
    let current_directory = env::current_dir().unwrap();
    let tree = TempTree::new();
    let mut relative_root = std::path::PathBuf::new();
    for _ in current_directory.ancestors().skip(1) {
        relative_root.push("..");
    }
    relative_root.push(tree.root.strip_prefix("/").unwrap());
    let session = tree.root.join("session.jsonl");
    fs::write(&session, b"session").unwrap();

    let report = PiSessionDiscovery::new(&relative_root).discover().unwrap();

    assert_eq!(
        report.coverage.inspected_roots,
        vec![current_directory.join(&relative_root)]
    );
    assert_eq!(
        report.files[0].path.canonicalize().unwrap(),
        session.canonicalize().unwrap()
    );
}

#[test]
fn discovery_recurses_and_returns_file_revisions_in_path_order() {
    let tree = TempTree::new();
    let project = tree.root.join("project");
    let nested = project.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let first = project.join("a.jsonl");
    let second = nested.join("b.jsonl");
    fs::write(&first, b"first").unwrap();
    fs::write(&second, b"second session").unwrap();
    fs::write(project.join("ignored.txt"), b"not a session").unwrap();
    fs::write(project.join("ignored.JSONL"), b"not a Pi session").unwrap();

    let report = PiSessionDiscovery::new(&tree.root).discover().unwrap();

    assert!(report.warnings.is_empty());
    assert_eq!(report.coverage.inspected_roots, vec![tree.root.clone()]);
    assert!(report.coverage.inaccessible_paths.is_empty());
    assert_eq!(
        report
            .files
            .iter()
            .map(|file| (&file.path, file.revision.size))
            .collect::<Vec<_>>(),
        vec![(&first, 5), (&second, 14)]
    );
    assert_eq!(
        report.files[0].revision.modified_at,
        fs::metadata(&first).unwrap().modified().unwrap()
    );
    assert_eq!(
        report.files[1].revision.modified_at,
        fs::metadata(&second).unwrap().modified().unwrap()
    );
}
