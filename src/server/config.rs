use std::{env, io, path::PathBuf};

pub struct ServerConfig {
    pub auth_file: PathBuf,
    pub database_path: PathBuf,
    pub max_upload_bytes: usize,
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let database_path = environment_path("TOKEN_TRACKER_SERVER_DATABASE")?
            .unwrap_or_else(|| PathBuf::from("/var/lib/token-tracker/server/snapshots.db"));
        Ok(Self {
            auth_file: environment_path("TOKEN_TRACKER_SERVER_AUTH_FILE")?
                .unwrap_or_else(|| PathBuf::from("/etc/token-tracker/auth.token")),
            database_path,
            max_upload_bytes: environment_size("TOKEN_TRACKER_MAX_UPLOAD_BYTES", 32 * 1024 * 1024)?,
        })
    }

    pub fn read_token(&self) -> io::Result<String> {
        crate::auth::read_token(&self.auth_file)
    }
}

fn environment_size(name: &str, default: usize) -> Result<usize, Box<dyn std::error::Error>> {
    match env::var(name) {
        Ok(value) => value
            .parse::<usize>()
            .ok()
            .filter(|size| *size > 0)
            .ok_or_else(|| io::Error::other(format!("{name} must be a positive integer")).into()),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(error.into()),
    }
}

fn environment_path(name: &str) -> io::Result<Option<PathBuf>> {
    env::var_os(name)
        .map(|value| {
            if value.is_empty() {
                Err(io::Error::other(format!("{name} must not be empty")))
            } else {
                Ok(PathBuf::from(value))
            }
        })
        .transpose()
}
