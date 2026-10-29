use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use super::{MAX_SNAPSHOT_BYTES, RemoteSnapshot, RemoteStore, Result, invalid};

pub struct SnapshotCache {
    directory: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheEntry {
    etag: String,
}

pub(crate) struct FileLock(File);

struct UncommittedSnapshots(Vec<PathBuf>);

impl Drop for UncommittedSnapshots {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub(crate) fn lock(path: &Path) -> Result<FileLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;

    file.try_lock_exclusive().map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            invalid("another sync or cache refresh is running")
        } else {
            error.into()
        }
    })?;

    Ok(FileLock(file))
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

impl SnapshotCache {
    pub fn new(root: &Path, endpoint: &str) -> Self {
        Self {
            directory: root.join(digest(endpoint)),
        }
    }

    fn path(&self, id: &str, etag: &str) -> PathBuf {
        self.directory.join(format!("{id}-{}.json", digest(etag)))
    }

    pub fn refresh(&self, remote: &impl RemoteStore) -> Result<Vec<RemoteSnapshot>> {
        fs::create_dir_all(&self.directory)?;
        let _lock = lock(&self.directory.join("cache.lock"))?;
        let listing = remote.list()?;

        let index_path = self.directory.join("index.json");
        let old: BTreeMap<String, CacheEntry> = match fs::read(&index_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };

        let mut index = BTreeMap::new();
        let mut snapshots = Vec::new();
        let mut uncommitted = UncommittedSnapshots(Vec::new());

        for object in listing {
            object.validate()?;
            let id = object.machine_id()?;
            if index.contains_key(id) {
                return Err(invalid("duplicate machine in remote listing"));
            }

            let cached = old
                .get(id)
                .filter(|entry| entry.etag == object.etag)
                .and_then(|entry| read_snapshot(&self.path(id, &entry.etag), id).ok());
            let (snapshot, etag) = if let Some(snapshot) = cached {
                (snapshot, object.etag.clone())
            } else {
                let mut temp = NamedTempFile::new_in(&self.directory)?;
                let etag = remote.download(&object.key, temp.as_file_mut())?;
                let snapshot = read_snapshot(temp.path(), id)?;

                temp.as_file_mut().flush()?;
                temp.as_file().sync_all()?;
                let path = self.path(id, &etag);
                temp.persist(&path).map_err(|error| error.error)?;
                if old.get(id).is_none_or(|entry| entry.etag != etag) {
                    uncommitted.0.push(path);
                }

                (snapshot, etag)
            };

            snapshots.push(snapshot);
            index.insert(id.to_owned(), CacheEntry { etag });
        }

        let mut temp = NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(temp.as_file_mut(), &index)?;
        temp.as_file().sync_all()?;
        temp.persist(index_path).map_err(|error| error.error)?;
        uncommitted.0.clear();

        for (id, entry) in old {
            if super::valid_machine_id(&id)
                && index.get(&id).is_none_or(|new| new.etag != entry.etag)
            {
                let _ = fs::remove_file(self.path(&id, &entry.etag));
            }
        }

        Ok(snapshots)
    }
}

fn read_snapshot(path: &Path, id: &str) -> Result<RemoteSnapshot> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_SNAPSHOT_BYTES {
        return Err(invalid("cached snapshot too large"));
    }

    let snapshot: RemoteSnapshot = serde_json::from_reader(std::io::BufReader::new(file))?;
    snapshot.validate()?;
    if snapshot.machine_id != id {
        return Err(invalid("snapshot machine ID does not match its key"));
    }

    Ok(snapshot)
}
