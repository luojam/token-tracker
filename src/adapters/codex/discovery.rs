use super::CODEX_AGENT_ID;
use crate::adapters::discovery::{self, RecursiveLayout};
use crate::adapters::files::{FileDiscoveryReport, SessionFileDiscovery};

use crate::domain::AgentId;
use std::path::{Component, Path, PathBuf};
use std::{env, error::Error, ffi::OsStr, fmt, io};

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

impl SessionFileDiscovery for CodexSessionDiscovery {
    type Error = CodexDiscoveryError;

    fn agent_id(&self) -> AgentId {
        AgentId::from(CODEX_AGENT_ID)
    }

    fn discover(&self) -> Result<FileDiscoveryReport, Self::Error> {
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

        Ok(discovery::discover(
            distinct_roots,
            RecursiveLayout(is_rollout),
        ))
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

fn is_rollout(path: &Path) -> bool {
    path.extension() == Some(OsStr::new("jsonl"))
        && path
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b"rollout-"))
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
