use super::PI_AGENT_ID;
use crate::adapters::discovery::{self, RecursiveLayout};
use crate::adapters::files::{FileDiscoveryReport, SessionFileDiscovery};

use crate::domain::AgentId;
use std::path::{Path, PathBuf};
use std::{env, error::Error, ffi::OsStr, fmt, io};

const PI_AGENT_DIRECTORY_ENV: &str = "PI_CODING_AGENT_DIR";
const PI_SESSION_DIRECTORY_ENV: &str = "PI_CODING_AGENT_SESSION_DIR";
const PI_CONFIG_DIRECTORY: &str = ".pi";
const PI_AGENT_DIRECTORY: &str = "agent";
const PI_SESSIONS_DIRECTORY: &str = "sessions";
const SESSION_EXTENSION: &str = "jsonl";

pub fn default_session_root() -> Result<PathBuf, PiDiscoveryError> {
    default_session_root_from(
        env::var_os(PI_SESSION_DIRECTORY_ENV).as_deref(),
        env::var_os(PI_AGENT_DIRECTORY_ENV).as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn default_session_root_from(
    session_directory: Option<&OsStr>,
    agent_directory: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, PiDiscoveryError> {
    let home_directory = || {
        home.filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or(PiDiscoveryError::HomeDirectoryUnavailable)
    };
    let resolve_override = |directory: &OsStr| {
        let directory = PathBuf::from(directory);
        match directory.strip_prefix("~") {
            Ok(relative) => home_directory().map(|home| home.join(relative)),
            Err(_) => Ok(directory),
        }
    };

    if let Some(directory) = session_directory.filter(|directory| !directory.is_empty()) {
        return resolve_override(directory);
    }

    let agent_directory = match agent_directory.filter(|directory| !directory.is_empty()) {
        Some(directory) => resolve_override(directory)?,
        None => home_directory()?
            .join(PI_CONFIG_DIRECTORY)
            .join(PI_AGENT_DIRECTORY),
    };

    Ok(agent_directory.join(PI_SESSIONS_DIRECTORY))
}

#[derive(Clone, Debug)]
pub struct PiSessionDiscovery {
    root: PathBuf,
}

impl PiSessionDiscovery {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn for_default_root() -> Result<Self, PiDiscoveryError> {
        default_session_root().map(Self::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SessionFileDiscovery for PiSessionDiscovery {
    type Error = PiDiscoveryError;

    fn agent_id(&self) -> AgentId {
        AgentId::from(PI_AGENT_ID)
    }

    fn discover(&self) -> Result<FileDiscoveryReport, Self::Error> {
        if self.root.as_os_str().is_empty() {
            return Err(PiDiscoveryError::EmptySessionRoot);
        }
        let root = std::path::absolute(&self.root)
            .map_err(|source| PiDiscoveryError::SessionRootResolution { source })?;

        Ok(discovery::discover(
            [root],
            RecursiveLayout(|path| path.extension() == Some(OsStr::new(SESSION_EXTENSION))),
        ))
    }
}

#[derive(Debug)]
pub enum PiDiscoveryError {
    EmptySessionRoot,
    HomeDirectoryUnavailable,
    SessionRootResolution { source: io::Error },
}

impl fmt::Display for PiDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySessionRoot => formatter.write_str("Pi session root is empty"),
            Self::HomeDirectoryUnavailable => {
                formatter.write_str("HOME is unavailable or is not an absolute path")
            }
            Self::SessionRootResolution { source } => {
                write!(formatter, "could not resolve Pi session root: {source}")
            }
        }
    }
}

impl Error for PiDiscoveryError {
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
    fn default_root_honors_the_directory_overrides() {
        let home = env::temp_dir().join("token-tracker-home");
        assert_eq!(
            default_session_root_from(None, None, Some(home.as_os_str())).unwrap(),
            home.join(".pi/agent/sessions")
        );

        let custom_agent_directory = home.join("custom-agent");
        assert_eq!(
            default_session_root_from(None, Some(custom_agent_directory.as_os_str()), None,)
                .unwrap(),
            custom_agent_directory.join("sessions")
        );
        assert_eq!(
            default_session_root_from(
                None,
                Some(OsStr::new("~/custom-agent")),
                Some(home.as_os_str()),
            )
            .unwrap(),
            home.join("custom-agent/sessions")
        );

        let custom_session_directory = home.join("current-sessions");
        assert_eq!(
            default_session_root_from(
                Some(custom_session_directory.as_os_str()),
                Some(custom_agent_directory.as_os_str()),
                None,
            )
            .unwrap(),
            custom_session_directory
        );
        assert_eq!(
            default_session_root_from(
                Some(OsStr::new("~/current-sessions")),
                None,
                Some(home.as_os_str()),
            )
            .unwrap(),
            home.join("current-sessions")
        );
        assert!(matches!(
            default_session_root_from(None, None, Some(OsStr::new("relative-home"))),
            Err(PiDiscoveryError::HomeDirectoryUnavailable)
        ));
    }
}
