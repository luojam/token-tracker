use std::{
    env,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

use crate::adapters::discovery::{self, DirectoryLayout};
use crate::application::DiscoveryWarning;

pub(super) enum DatabaseLocations {
    Explicit(Vec<PathBuf>),
    Home { root: PathBuf, profiles: bool },
}

impl DatabaseLocations {
    pub fn from_environment() -> io::Result<Self> {
        let home_directory = || {
            env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "HOME is unavailable or is not an absolute path",
                    )
                })
        };

        if let Some(home) = env::var_os("HERMES_HOME").filter(|home| !home.is_empty()) {
            let home = PathBuf::from(home);
            let root = match home.strip_prefix("~") {
                Ok(relative) => home_directory()?.join(relative),
                Err(_) => std::path::absolute(home)?,
            };
            return Ok(Self::Home {
                root,
                profiles: false,
            });
        }

        let home = home_directory()?;
        let mut directory = OsString::from(".hermes");
        if let Some(suffix) = env::var_os("HERMES_DATA_DIR_SUFFIX") {
            directory.push(suffix);
        }

        Ok(Self::Home {
            root: home.join(directory),
            profiles: true,
        })
    }

    pub fn discover(&self) -> (Vec<PathBuf>, Vec<DiscoveryWarning>, bool) {
        match self {
            Self::Explicit(paths) => (paths.clone(), Vec::new(), true),
            Self::Home { root, profiles } => {
                let layout = if *profiles {
                    Layout::Root
                } else {
                    Layout::Profile
                };
                let report = discovery::discover([root.clone()], layout);
                (
                    report.files.into_iter().map(|file| file.path).collect(),
                    report.warnings,
                    report.coverage.inaccessible_paths.is_empty(),
                )
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Layout {
    Root,
    Profiles,
    Profile,
}

impl DirectoryLayout for Layout {
    fn child_directory(&self, name: &OsStr) -> Option<Self> {
        match self {
            Self::Root if name == "profiles" => Some(Self::Profiles),
            Self::Profiles => Some(Self::Profile),
            _ => None,
        }
    }

    fn is_source_file(&self, path: &Path) -> bool {
        matches!(self, Self::Root | Self::Profile)
            && path.file_name() == Some(OsStr::new("state.db"))
    }
}
