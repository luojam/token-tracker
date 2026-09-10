use super::CLAUDE_AGENT_ID;
use crate::application::{
    DiscoveredSessionFile, DiscoveryCoverage, DiscoveryReport, DiscoveryWarning, FileRevision,
    SessionDiscovery,
};
use crate::core::AgentId;
use std::path::{Path, PathBuf};
use std::{env, error::Error, ffi::OsStr, fmt, fs, io};

pub fn default_session_root() -> Result<PathBuf, ClaudeDiscoveryError> {
    default_session_root_from(
        env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn default_session_root_from(
    config_directory: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, ClaudeDiscoveryError> {
    let directory = match config_directory.filter(|directory| !directory.is_empty()) {
        Some(directory) => PathBuf::from(directory),
        None => home
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or(ClaudeDiscoveryError::HomeDirectoryUnavailable)?
            .join(".claude"),
    };
    Ok(absolute_root(&directory)?.join("projects"))
}

#[derive(Clone, Debug)]
pub struct ClaudeSessionDiscovery {
    root: PathBuf,
}

impl ClaudeSessionDiscovery {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn for_default_root() -> Result<Self, ClaudeDiscoveryError> {
        default_session_root().map(Self::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SessionDiscovery for ClaudeSessionDiscovery {
    type Error = ClaudeDiscoveryError;

    fn agent_id(&self) -> AgentId {
        AgentId::from(CLAUDE_AGENT_ID)
    }

    fn discover(&self) -> Result<DiscoveryReport, Self::Error> {
        let root = absolute_root(&self.root)?;
        let mut report = DiscoveryReport {
            files: Vec::new(),
            warnings: Vec::new(),
            coverage: DiscoveryCoverage {
                inspected_roots: Vec::new(),
                inaccessible_paths: Vec::new(),
            },
        };
        let mut pending = vec![(root.clone(), Layout::Root)];
        while let Some((directory, layout)) = pending.pop() {
            let inspected = scan_directory(&directory, layout, &mut pending, &mut report);
            if directory == root && inspected {
                report.coverage.inspected_roots.push(root.clone());
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

#[derive(Clone, Copy)]
enum Layout {
    Root,
    Project,
    Session,
    Subagents,
}

impl Layout {
    fn child_directory(self, name: &OsStr) -> Option<Self> {
        match self {
            Self::Root => Some(Self::Project),
            Self::Project if is_session_id(name) => Some(Self::Session),
            Self::Session if name == "subagents" => Some(Self::Subagents),
            Self::Subagents => Some(Self::Subagents),
            _ => None,
        }
    }

    fn is_transcript(self, path: &Path) -> bool {
        if path.extension() != Some(OsStr::new("jsonl")) {
            return false;
        }
        let Some(stem) = path.file_stem() else {
            return false;
        };
        match self {
            Self::Project => is_session_id(stem),
            Self::Subagents => stem
                .to_str()
                .and_then(|stem| stem.strip_prefix("agent-"))
                .is_some_and(|id| !id.is_empty() && !id.contains([':', '/', '\\'])),
            _ => false,
        }
    }
}

fn is_session_id(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn absolute_root(root: &Path) -> Result<PathBuf, ClaudeDiscoveryError> {
    if root.as_os_str().is_empty() {
        return Err(ClaudeDiscoveryError::EmptySessionRoot);
    }
    std::path::absolute(root)
        .map(|root| root.components().collect())
        .map_err(|source| ClaudeDiscoveryError::SessionRootResolution { source })
}

// False means the directory cannot establish absence for retained sources.
fn scan_directory(
    directory: &Path,
    layout: Layout,
    pending: &mut Vec<(PathBuf, Layout)>,
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
        let candidate = layout.is_transcript(&path);
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

#[derive(Debug)]
pub enum ClaudeDiscoveryError {
    EmptySessionRoot,
    HomeDirectoryUnavailable,
    SessionRootResolution { source: io::Error },
}

impl fmt::Display for ClaudeDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySessionRoot => formatter.write_str("Claude session root is empty"),
            Self::HomeDirectoryUnavailable => {
                formatter.write_str("HOME is unavailable or is not an absolute path")
            }
            Self::SessionRootResolution { source } => {
                write!(formatter, "could not resolve Claude session root: {source}")
            }
        }
    }
}

impl Error for ClaudeDiscoveryError {
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
    fn root_uses_nonempty_override_or_absolute_home() {
        let home = env::temp_dir().join("token-tracker-claude-home");
        for config in [None, Some(OsStr::new(""))] {
            assert_eq!(
                default_session_root_from(config, Some(home.as_os_str())).unwrap(),
                home.join(".claude/projects")
            );
        }
        assert_eq!(
            default_session_root_from(Some(home.as_os_str()), None).unwrap(),
            home.join("projects")
        );
        assert_eq!(
            default_session_root_from(Some(OsStr::new("custom-claude")), None).unwrap(),
            env::current_dir().unwrap().join("custom-claude/projects")
        );
        for home in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("relative-home")),
        ] {
            assert!(matches!(
                default_session_root_from(None, home),
                Err(ClaudeDiscoveryError::HomeDirectoryUnavailable)
            ));
        }
        assert!(matches!(
            ClaudeSessionDiscovery::new("").discover(),
            Err(ClaudeDiscoveryError::EmptySessionRoot)
        ));
    }
}
