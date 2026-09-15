use std::{env, fs, io, path::PathBuf};

use serde::Deserialize;

use super::CliError;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub machine_name: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self, CliError> {
        let absolute_env_path = |name| {
            env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let Some(directory) = absolute_env_path("XDG_CONFIG_HOME")
            .or_else(|| absolute_env_path("HOME").map(|home| home.join(".config")))
        else {
            return Ok(Self::default());
        };
        let path = directory.join("token-tracker/config.toml");
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(CliError::Config { path, source }),
        };
        toml::from_str(&content).map_err(|source| CliError::Config {
            path,
            source: io::Error::new(io::ErrorKind::InvalidData, source),
        })
    }
}
