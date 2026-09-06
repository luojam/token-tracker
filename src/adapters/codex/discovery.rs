use super::CODEX_AGENT_ID;
use crate::application::{
    DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, DiscoveryWarning, FileRevision,
    SessionDiscovery,
};
use crate::core::AgentId;
use std::path::{Component, Path, PathBuf};
use std::{env, error::Error, ffi::OsStr, fmt, fs, io};

pub fn default_session_roots() -> Result<Vec<PathBuf>, CodexDiscoveryError> {
    default_session_roots_from(
        env::var_os("CODEX_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn default_session_roots_from(
    codex_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<Vec<PathBuf>, CodexDiscoveryError> {
    let directory = match codex_home.filter(|directory| !directory.is_empty()) {
        Some(directory) => PathBuf::from(directory),
        None => home
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or(CodexDiscoveryError::HomeDirectoryUnavailable)?
            .join(".codex"),
    };
    let directory = absolute_root(&directory)?;
    Ok(vec![
        directory.join("sessions"),
        directory.join("archived_sessions"),
    ])
}

#[derive(Clone, Debug)]
pub struct CodexSessionDiscovery {
    roots: Vec<PathBuf>,
}

impl CodexSessionDiscovery {
    pub fn new(roots: impl IntoIterator<Item = impl Into<PathBuf>>) -> Self {
        Self {
            roots: roots.into_iter().map(Into::into).collect(),
        }
    }

    pub fn for_default_roots() -> Result<Self, CodexDiscoveryError> {
        default_session_roots().map(Self::new)
    }
}

impl SessionDiscovery for CodexSessionDiscovery {
    type Error = CodexDiscoveryError;

    fn agent_id(&self) -> AgentId {
        AgentId::from(CODEX_AGENT_ID)
    }

    fn discover(&self) -> Result<DiscoveryReport, Self::Error> {
        let mut roots = self
            .roots
            .iter()
            .map(|root| absolute_root(root))
            .collect::<Result<Vec<_>, _>>()?;
        roots.sort_unstable();
        let mut distinct_roots: Vec<PathBuf> = Vec::new();
        for root in roots {
            if !distinct_roots.iter().any(|ancestor| {
                root.strip_prefix(ancestor).is_ok_and(|suffix| {
                    !suffix.components().any(|part| part == Component::ParentDir)
                })
            }) {
                distinct_roots.push(root);
            }
        }

        let mut report = DiscoveryReport {
            files: Vec::new(),
            warnings: Vec::new(),
            coverage: DiscoveryCoverage {
                inspected_roots: Vec::new(),
                inaccessible_paths: Vec::new(),
            },
        };
        for root in distinct_roots {
            let mut pending = vec![root.clone()];
            while let Some(directory) = pending.pop() {
                let inspected = scan_directory(&directory, &mut pending, &mut report);
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
        Ok(report)
    }
}

fn absolute_root(root: &Path) -> Result<PathBuf, CodexDiscoveryError> {
    if root.as_os_str().is_empty() {
        return Err(CodexDiscoveryError::EmptySessionRoot);
    }
    std::path::absolute(root)
        .map(|root| root.components().collect())
        .map_err(|source| CodexDiscoveryError::SessionRootResolution { source })
}

// False means the directory cannot establish absence for retained sources.
fn scan_directory(
    directory: &Path,
    pending: &mut Vec<PathBuf>,
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
        if file_type.is_dir() {
            pending.push(path);
            continue;
        }
        let candidate = is_rollout(&path);
        if !candidate && !file_type.is_symlink() {
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

fn is_rollout(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("jsonl"))
        && path
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b"rollout-"))
}

fn record_inaccessible(report: &mut DiscoveryReport, path: &Path, message: String) {
    report.coverage.inaccessible_paths.push(path.to_owned());
    report.warnings.push(DiscoveryWarning {
        path: Some(path.to_owned()),
        message,
    });
}

#[derive(Debug)]
pub enum CodexDiscoveryError {
    EmptySessionRoot,
    HomeDirectoryUnavailable,
    SessionRootResolution { source: io::Error },
}

impl fmt::Display for CodexDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySessionRoot => formatter.write_str("Codex session root is empty"),
            Self::HomeDirectoryUnavailable => {
                formatter.write_str("HOME is unavailable or is not an absolute path")
            }
            Self::SessionRootResolution { source } => {
                write!(formatter, "could not resolve Codex session root: {source}")
            }
        }
    }
}

impl Error for CodexDiscoveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::SessionRootResolution { source } => Some(source),
            Self::EmptySessionRoot | Self::HomeDirectoryUnavailable => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_use_nonempty_override_or_absolute_home() {
        let home = env::temp_dir().join("token-tracker-codex-home");
        let expected = vec![
            home.join(".codex/sessions"),
            home.join(".codex/archived_sessions"),
        ];
        for override_home in [None, Some(OsStr::new(""))] {
            assert_eq!(
                default_session_roots_from(override_home, Some(home.as_os_str())).unwrap(),
                expected
            );
        }
        assert_eq!(
            default_session_roots_from(Some(home.as_os_str()), None).unwrap(),
            vec![home.join("sessions"), home.join("archived_sessions")]
        );
        let relative = env::current_dir().unwrap().join("custom-codex");
        assert_eq!(
            default_session_roots_from(
                Some(OsStr::new("custom-codex")),
                Some(OsStr::new("invalid-home"))
            )
            .unwrap(),
            vec![
                relative.join("sessions"),
                relative.join("archived_sessions")
            ]
        );
        for home in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("relative-home")),
        ] {
            assert!(matches!(
                default_session_roots_from(None, home),
                Err(CodexDiscoveryError::HomeDirectoryUnavailable)
            ));
        }
        assert!(matches!(
            CodexSessionDiscovery::new([""]).discover(),
            Err(CodexDiscoveryError::EmptySessionRoot)
        ));
    }
}
