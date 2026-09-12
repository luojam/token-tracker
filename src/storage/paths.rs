use super::SqliteStoreError;
use std::{env, ffi::OsStr, path::PathBuf};

const APPLICATION_DIRECTORY: &str = "token-tracker";
const DATABASE_FILENAME: &str = "usage.db";

pub fn default_database_path() -> Result<PathBuf, SqliteStoreError> {
    default_database_path_from(
        env::var_os("XDG_DATA_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

pub(super) fn default_database_path_from(
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, SqliteStoreError> {
    let data_home = match absolute_environment_path(xdg_data_home) {
        Some(path) => path,
        None => absolute_environment_path(home)
            .ok_or(SqliteStoreError::HomeDirectoryUnavailable)?
            .join(".local")
            .join("share"),
    };

    Ok(data_home
        .join(APPLICATION_DIRECTORY)
        .join(DATABASE_FILENAME))
}

fn absolute_environment_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_prefers_xdg_and_falls_back_to_home() {
        assert_eq!(
            default_database_path_from(Some(OsStr::new("/data")), Some(OsStr::new("/home/me")))
                .unwrap(),
            PathBuf::from("/data/token-tracker/usage.db")
        );
        assert_eq!(
            default_database_path_from(None, Some(OsStr::new("/home/me"))).unwrap(),
            PathBuf::from("/home/me/.local/share/token-tracker/usage.db")
        );
        assert_eq!(
            default_database_path_from(
                Some(OsStr::new("relative-data")),
                Some(OsStr::new("/home/me")),
            )
            .unwrap(),
            PathBuf::from("/home/me/.local/share/token-tracker/usage.db")
        );
    }

    #[test]
    fn default_path_rejects_an_invalid_home() {
        for home in [OsStr::new(""), OsStr::new("relative-home")] {
            assert!(matches!(
                default_database_path_from(None, Some(home)),
                Err(SqliteStoreError::HomeDirectoryUnavailable)
            ));
        }
    }
}
