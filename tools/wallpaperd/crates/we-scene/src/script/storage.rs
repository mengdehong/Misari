//! SceneScript storage is a bounded JSON map, scoped by project and output.
//! A per-map lock serializes processes; atomic replacement preserves old data on failure.
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const LIMIT: usize = 256 * 1024;
#[derive(Clone)]
pub struct ScriptStorage {
    pub directory: PathBuf,
    pub output: String,
}
pub(super) struct Storage {
    paths: Option<[PathBuf; 2]>,
    memory: [Map<String, Value>; 2],
}
impl Storage {
    pub fn new(project: &Path, config: Option<ScriptStorage>) -> Self {
        let paths = config.map(|c| {
            let hash = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
            let directory = c
                .directory
                .join(hash(project.as_os_str().as_encoded_bytes()));
            [
                directory.join(format!("screen-{}.json", hash(c.output.as_bytes()))),
                directory.join("global.json"),
            ]
        });
        Self {
            paths,
            memory: std::array::from_fn(|_| Map::new()),
        }
    }
    pub fn apply(
        &mut self,
        action: &str,
        key: &str,
        data: &str,
        location: &str,
    ) -> Result<Option<String>> {
        let index = match location {
            "screen" => 0,
            "global" => 1,
            _ => anyhow::bail!("invalid storage location"),
        };
        ensure!(key.len() <= 256, "storage key exceeds 256 bytes");
        ensure!(data.len() <= LIMIT, "storage value exceeds 256 KiB");
        let mut lock = None;
        let mut values = if let Some(paths) = &self.paths {
            let path = &paths[index];
            let parent = path.parent().unwrap();
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
            ensure!(
                fs::symlink_metadata(parent)?.is_dir(),
                "storage directory is not a directory"
            );
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(path.with_extension("lock"))?;
            ensure!(
                file.metadata()?.is_file(),
                "storage lock is not a regular file"
            );
            file.try_lock().context("SceneScript storage is busy")?;
            lock = Some(file);
            match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                .open(path)
            {
                Ok(file) => {
                    ensure!(
                        file.metadata()?.is_file() && file.metadata()?.len() <= LIMIT as u64,
                        "invalid storage file"
                    );
                    let mut bytes = Vec::new();
                    file.take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
                    ensure!(bytes.len() <= LIMIT, "storage exceeds 256 KiB");
                    serde_json::from_slice(&bytes).context("corrupt SceneScript storage")?
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
                Err(e) => return Err(e.into()),
            }
        } else {
            self.memory[index].clone()
        };
        let result = match action {
            "get" => {
                return values
                    .get(key)
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(Into::into);
            }
            "set" => {
                values.insert(key.into(), serde_json::from_str(data)?);
                None
            }
            "delete" => Some(values.remove(key).is_some().to_string()),
            "clear" => {
                values.clear();
                None
            }
            _ => anyhow::bail!("invalid storage operation"),
        };
        ensure!(values.len() <= 256, "storage exceeds 256 keys");
        let bytes = serde_json::to_vec(&values)?;
        ensure!(bytes.len() <= LIMIT, "storage exceeds 256 KiB");
        if let Some(paths) = &self.paths {
            let path = &paths[index];
            let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
            temporary.write_all(&bytes)?;
            temporary.persist(path).map_err(|e| e.error)?;
        } else {
            self.memory[index] = values;
        }
        drop(lock);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_scope_global_sharing_and_failed_write_preserve_old_value() {
        let root = tempfile::tempdir().unwrap();
        let config = |output: &str| {
            Some(ScriptStorage {
                directory: root.path().join("storage"),
                output: output.into(),
            })
        };
        let mut a = Storage::new(Path::new("project"), config("A"));
        let mut b = Storage::new(Path::new("project"), config("B"));
        a.apply("set", "key", "[1,2,3]", "screen").unwrap();
        assert!(b.apply("get", "key", "", "screen").unwrap().is_none());
        a.apply("set", "key", "42", "global").unwrap();
        assert_eq!(b.apply("get", "key", "", "global").unwrap().unwrap(), "42");
        let mut reload = Storage::new(Path::new("project"), config("A"));
        assert_eq!(
            reload.apply("get", "key", "", "screen").unwrap().unwrap(),
            "[1,2,3]"
        );
        assert!(
            reload
                .apply("set", "key", &"0".repeat(LIMIT + 1), "screen")
                .is_err()
        );
        assert_eq!(
            a.apply("get", "key", "", "screen").unwrap().unwrap(),
            "[1,2,3]"
        );
        let mut other = Storage::new(Path::new("other"), config("A"));
        assert!(other.apply("get", "key", "", "global").unwrap().is_none());
        assert!(a.apply("get", "key", "", "../escape").is_err());
        assert_eq!(
            a.apply("delete", "key", "", "screen").unwrap().unwrap(),
            "true"
        );
    }
    #[test]
    fn storage_rejects_symlinks_pipes_and_contended_locks_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        let mut storage = Storage::new(
            Path::new("project"),
            Some(ScriptStorage {
                directory: root.path().into(),
                output: "A".into(),
            }),
        );
        storage.apply("set", "k", "1", "screen").unwrap();
        let path = storage.paths.as_ref().unwrap()[0].clone();
        let lock_path = path.with_extension("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        file.lock().unwrap();
        assert!(storage.apply("set", "k", "2", "screen").is_err());
        drop(file);
        assert_eq!(
            storage.apply("get", "k", "", "screen").unwrap().unwrap(),
            "1"
        );
        fs::remove_file(&path).unwrap();
        let outside = root.path().join("outside");
        fs::write(&outside, b"{\"k\":42}").unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(storage.apply("get", "k", "", "screen").is_err());
        fs::remove_file(&path).unwrap();
        let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: creates only a FIFO inside the owned temporary test directory.
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        assert!(storage.apply("get", "k", "", "screen").is_err());
        assert_eq!(fs::read(outside).unwrap(), b"{\"k\":42}");
    }
}
