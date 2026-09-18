use std::{
    env,
    fs::File,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub struct ServerConfig {
    pub auth_file: PathBuf,
    pub database_path: PathBuf,
    pub max_upload_bytes: usize,
}

impl ServerConfig {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let database_path = environment_path("TOKEN_TRACKER_SERVER_DATABASE")?
            .unwrap_or_else(|| PathBuf::from("/var/lib/token-tracker/snapshots.db"));
        Ok(Self {
            auth_file: environment_path("TOKEN_TRACKER_SERVER_AUTH_FILE")?
                .unwrap_or_else(|| PathBuf::from("/etc/token-tracker/auth.token")),
            database_path,
            max_upload_bytes: environment_size("TOKEN_TRACKER_MAX_UPLOAD_BYTES", 32 * 1024 * 1024)?,
        })
    }

    pub fn read_token(&self) -> io::Result<String> {
        read_token(&self.auth_file)
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

pub(super) fn valid_token(token: &str) -> bool {
    (32..=4096).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
}

fn read_token(path: &Path) -> io::Result<String> {
    let invalid = |message| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("auth file {}: {message}", path.display()),
        )
    };
    let file = File::open(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("auth file {}: {error}", path.display()),
        )
    })?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid(
            "must be a regular file accessible only by its owner (chmod 600)",
        ));
    }
    let mut contents = String::new();
    file.take(4097).read_to_string(&mut contents)?;
    let token = contents.trim_end_matches(['\r', '\n']);
    if contents.len() > 4096 || !valid_token(token) {
        return Err(invalid(
            "must contain one bearer token of at least 32 characters, at most 4096 bytes including its newline",
        ));
    }
    Ok(token.to_owned())
}
