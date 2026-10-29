use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use serde::Deserialize;

use super::CliError;

const TEMPLATE: &str = "machine_name = \"\"\n";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub machine_name: Option<String>,
    pub remote_url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Auth {
    remote_token: String,
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
        Ok(config)
    }

    pub fn remote_token() -> Result<String, CliError> {
        let path = Self::path()
            .ok_or_else(|| CliError::Config {
                path: PathBuf::from("auth.toml"),
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "no configuration directory; set HOME or XDG_CONFIG_HOME",
                ),
            })?
            .with_file_name("auth.toml");
        read_auth_token(&path).map_err(|source| CliError::Config { path, source })
    }
}

fn read_auth_token(path: &Path) -> io::Result<String> {
    let content = fs::read_to_string(path)?;
    let auth: Auth = toml::from_str(&content).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "expected remote_token = \"...\" in auth.toml",
        )
    })?;
    if auth.remote_token.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote_token must not be empty",
        ));
    }
    Ok(auth.remote_token)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_loads_the_token_without_changing_the_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("auth.toml");
        let content = "# Remote credentials\nremote_token = 'SECRET_token'\n";
        fs::write(&path, content).unwrap();

        assert_eq!(read_auth_token(&path).unwrap(), "SECRET_token");
        assert_eq!(fs::read_to_string(path).unwrap(), content);
    }
}
