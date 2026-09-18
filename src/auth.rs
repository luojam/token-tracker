use std::{
    fs::File,
    io::{self, Read},
    os::unix::fs::PermissionsExt,
    path::Path,
};

pub(crate) fn valid_token(token: &str) -> bool {
    (32..=4096).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte))
}

pub fn read_token(path: &Path) -> io::Result<String> {
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
