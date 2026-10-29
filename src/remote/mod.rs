pub(crate) mod cache;
mod snapshot;
mod transport;

pub use cache::SnapshotCache;
pub use snapshot::{RemoteSnapshot, SnapshotCosts, SnapshotRow, build_snapshot, combine_snapshots};
pub use transport::{HttpRemote, ObjectMetadata, RemoteStore};

pub const MAX_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;
pub type Result<T> = std::result::Result<T, RemoteError>;

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("{0}")]
    Invalid(String),

    #[error("remote snapshot changed during sync; run sync again to rebuild from fresh state")]
    Conflict,

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Http(#[from] reqwest::Error),

    #[error("{0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Storage(#[from] crate::storage::SqliteStoreError),

    #[error("{0}")]
    Import(#[from] crate::application::ImportSynchronizationError),

    #[error("{0}")]
    Summary(#[from] crate::application::UsageSummaryError),

    #[error("{0}")]
    Deduplication(#[from] crate::application::DeduplicationError),
}

fn invalid(message: impl Into<String>) -> RemoteError {
    RemoteError::Invalid(message.into())
}

pub fn valid_machine_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after Unix epoch")
        .as_millis() as i64
}
