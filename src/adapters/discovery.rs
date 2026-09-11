use crate::application::{
    DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, DiscoveryWarning, FileRevision,
};
use std::path::{Path, PathBuf};
use std::{ffi::OsStr, fs, io};

pub(super) trait DirectoryLayout: Sized {
    /// None excludes this directory from the adapter's layout.
    fn child_directory(&self, name: &OsStr) -> Option<Self>;
    fn is_session_file(&self, path: &Path) -> bool;
}

#[derive(Clone, Copy)]
pub(super) struct RecursiveLayout(pub fn(&Path) -> bool);

impl DirectoryLayout for RecursiveLayout {
    fn child_directory(&self, _name: &OsStr) -> Option<Self> {
        Some(*self)
    }

    fn is_session_file(&self, path: &Path) -> bool {
        (self.0)(path)
    }
}

pub(super) fn discover<L: DirectoryLayout + Clone>(
    roots: impl IntoIterator<Item = PathBuf>,
    layout: L,
) -> DiscoveryReport {
    let mut report = DiscoveryReport {
        files: Vec::new(),
        warnings: Vec::new(),
        coverage: DiscoveryCoverage {
            inspected_roots: Vec::new(),
            inaccessible_paths: Vec::new(),
        },
    };
    for root in roots {
        let root: PathBuf = root.components().collect();
        let mut pending = vec![(root.clone(), layout.clone())];
        while let Some((directory, layout)) = pending.pop() {
            let inspected = scan_directory(&directory, layout, &mut pending, &mut report);
            if directory == root && inspected {
                report.coverage.inspected_roots.push(root.clone());
            }
        }
    }

    report
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    report.coverage.inaccessible_paths.sort_unstable();
    report.coverage.inaccessible_paths.dedup();
    report.warnings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.message.cmp(&right.message))
    });
    report
}

// False prevents marking retained sources under this directory as missing.
fn scan_directory<L: DirectoryLayout>(
    directory: &Path,
    layout: L,
    pending: &mut Vec<(PathBuf, L)>,
    report: &mut DiscoveryReport,
) -> bool {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            record_inaccessible(
                report,
                directory,
                "directory symlinks are not inspected".into(),
            );
            return false;
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(error) => {
            record_inaccessible(
                report,
                directory,
                format!("could not inspect directory: {error}"),
            );
            return false;
        }
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return true,
        Err(error) => {
            record_inaccessible(
                report,
                directory,
                format!("could not read directory: {error}"),
            );
            return false;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                record_inaccessible(
                    report,
                    directory,
                    format!("could not read directory entry: {error}"),
                );
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                record_inaccessible(report, &path, format!("could not inspect path: {error}"));
                continue;
            }
        };
        let child_layout = layout.child_directory(&entry.file_name());
        if file_type.is_dir() {
            if let Some(child_layout) = child_layout {
                pending.push((path, child_layout));
            }
            continue;
        }
        let candidate = layout.is_session_file(&path);
        if !candidate && !(file_type.is_symlink() && child_layout.is_some()) {
            continue;
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                if candidate {
                    record_inaccessible(
                        report,
                        &path,
                        format!("could not read file metadata: {error}"),
                    );
                } else {
                    // An unresolved link could have been a directory in a prior scan.
                    record_inaccessible(
                        report,
                        &path,
                        format!("could not inspect symlink target: {error}"),
                    );
                }
                continue;
            }
        };
        if metadata.is_dir() {
            record_inaccessible(report, &path, "directory symlinks are not inspected".into());
            continue;
        }
        if !candidate || !metadata.is_file() {
            continue;
        }
        let modified_at = match metadata.modified() {
            Ok(modified_at) => modified_at,
            Err(error) => {
                record_inaccessible(
                    report,
                    &path,
                    format!("could not read modification time: {error}"),
                );
                continue;
            }
        };
        report.files.push(DiscoveredSessionFile {
            path,
            revision: FileRevision {
                size: metadata.len(),
                modified_at,
            },
        });
    }
    true
}

fn record_inaccessible(report: &mut DiscoveryReport, path: &Path, message: String) {
    report.coverage.inaccessible_paths.push(path.to_owned());
    report.warnings.push(DiscoveryWarning {
        path: Some(path.to_owned()),
        message,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::UsageStore;
    use crate::domain::{AgentId, Timestamp};
    use crate::storage::SqliteUsageStore;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TREE: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "token-tracker-discovery-{}-{}",
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

    fn scan(root: &Path) -> DiscoveryReport {
        discover(
            [root.to_owned()],
            RecursiveLayout(|path| path.extension() == Some(OsStr::new("jsonl"))),
        )
    }

    #[test]
    fn skipped_links_preserve_presence_while_readable_files_are_discovered() {
        let tree = TempTree::new();
        let root = tree.0.join("sessions");
        let project = root.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("session.jsonl"), b"session").unwrap();
        let agent = AgentId::from("test");
        let mut store = SqliteUsageStore::open_in_memory().unwrap();
        store
            .record_discovery(&agent, &scan(&root), Timestamp::from_unix_milliseconds(1))
            .unwrap();

        let target = tree.0.join("outside");
        fs::rename(&project, &target).unwrap();
        symlink(&target, &project).unwrap();
        symlink(&target, root.join("directory.jsonl")).unwrap();
        symlink(&root, root.join("cycle")).unwrap();
        for name in ["broken", "broken.jsonl"] {
            symlink(tree.0.join("missing"), root.join(name)).unwrap();
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
        store
            .record_discovery(&agent, &report, Timestamp::from_unix_milliseconds(2))
            .unwrap();
        let states = store.source_states(&agent).unwrap();
        assert_eq!(states.len(), 2);
        assert!(states.iter().all(|source| source.present));

        fs::remove_dir_all(&root).unwrap();
        store
            .record_discovery(&agent, &scan(&root), Timestamp::from_unix_milliseconds(3))
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
    fn symlink_and_unreadable_roots_do_not_establish_absence() {
        let tree = TempTree::new();
        let root = tree.0.join("root");
        symlink(&tree.0, &root).unwrap();
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

    #[test]
    fn unreadable_subtree_is_excluded_from_coverage() {
        let tree = TempTree::new();
        let blocked = tree.0.join("blocked");
        fs::create_dir(&blocked).unwrap();
        let candidate = blocked.join("session.jsonl");
        fs::write(&candidate, b"session").unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
        let report = scan(&tree.0);
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
        // Privileged runners can still read mode-000 directories.
        if report.files.iter().any(|file| file.path == candidate) {
            return;
        }
        assert!(report.files.is_empty());
        assert_eq!(report.coverage.inspected_roots, vec![tree.0.clone()]);
        assert_eq!(report.coverage.inaccessible_paths, vec![blocked.clone()]);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(report.warnings[0].path.as_ref(), Some(&blocked));
    }
}
