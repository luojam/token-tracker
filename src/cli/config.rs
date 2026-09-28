use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::CliError;

const TEMPLATE: &str = "machine_name = \"\"\nserver_url = \"\"\nauth_file = \"\"\n";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub machine_name: Option<String>,
    pub server_url: Option<String>,
    pub auth_file: Option<PathBuf>,
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        let absolute_env_path = |name| {
            env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        absolute_env_path("XDG_CONFIG_HOME")
            .or_else(|| absolute_env_path("HOME").map(|home| home.join(".config")))
            .map(|directory| directory.join("token-tracker/config.toml"))
    }

    pub fn load() -> Result<Self, CliError> {
        let Some(path) = Self::path() else {
            return Ok(Self::default());
        };
        let content = read_or_create(&path).map_err(|source| CliError::Config {
            path: path.clone(),
            source,
        })?;
        Self::parse(&content, path)
    }

    pub fn parse(content: &str, path: PathBuf) -> Result<Self, CliError> {
        let mut config: Self = toml::from_str(content).map_err(|source| CliError::Config {
            path,
            source: io::Error::new(io::ErrorKind::InvalidData, source.message()),
        })?;
        config.machine_name = config.machine_name.filter(|value| !value.is_empty());
        config.server_url = config.server_url.filter(|value| !value.is_empty());
        config.auth_file = config
            .auth_file
            .filter(|value| !value.as_os_str().is_empty());
        Ok(config)
    }
}

fn read_or_create(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => return Ok(content),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::create_dir_all(path.parent().unwrap())?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(TEMPLATE.as_bytes())?;
            Ok(TEMPLATE.into())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => fs::read_to_string(path),
        Err(error) => Err(error),
    }
}
