use crate::adapters::files::{
    DiscoveredSessionFile, FileDiscoveryCoverage, FileDiscoveryReport, FileRevision,
};
use crate::application::DiscoveryWarning;
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
) -> FileDiscoveryReport {
    let mut report = FileDiscoveryReport {
        files: Vec::new(),
        warnings: Vec::new(),
        coverage: FileDiscoveryCoverage {
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
    report: &mut FileDiscoveryReport,
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

fn record_inaccessible(report: &mut FileDiscoveryReport, path: &Path, message: String) {
    report.coverage.inaccessible_paths.push(path.to_owned());
    report.warnings.push(DiscoveryWarning {
        path: Some(path.to_owned()),
        message,
    });
}
