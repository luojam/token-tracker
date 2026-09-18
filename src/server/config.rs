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
        let max_upload_bytes = match env::var("TOKEN_TRACKER_MAX_UPLOAD_BYTES") {
            Ok(value) => value
                .parse::<usize>()
                .ok()
                .filter(|size| *size > 0)
                .ok_or_else(|| {
                    io::Error::other("TOKEN_TRACKER_MAX_UPLOAD_BYTES must be a positive integer")
                })?,
            Err(env::VarError::NotPresent) => 32 * 1024 * 1024,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            auth_file: PathBuf::from("/etc/token-tracker/auth.token"),
            database_path,
            max_upload_bytes,
        })
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

pub(super) fn read_token(path: &Path) -> io::Result<String> {
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
    if contents.len() > 4096
        || token.len() < 32
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
    {
        return Err(invalid(
            "must contain one bearer token of at least 32 characters, at most 4096 bytes including its newline",
        ));
    }
    Ok(token.to_owned())
}
