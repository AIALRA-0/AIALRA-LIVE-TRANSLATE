//! Content-addressed storage for audio chunks, originals, page images, and exports.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

#[derive(Debug, Clone)]
pub struct ObjectStore {
    root: PathBuf,
}

impl ObjectStore {
    /// The root is resolved once so later object identifiers cannot escape the data directory.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).context("create object store root")?;
        Ok(Self { root })
    }

    /// Objects are atomically persisted under their SHA-256 digest and deduplicated by content.
    pub fn put(&self, bytes: &[u8]) -> Result<StoredObject> {
        let digest = Sha256::digest(bytes);
        let hash = hex::encode(digest);
        let prefix = &hash[..2];
        let dir = self.root.join(prefix);
        fs::create_dir_all(&dir).context("create object prefix directory")?;
        let path = dir.join(&hash);

        if !path.exists() {
            let mut temp = NamedTempFile::new_in(&dir).context("create temporary object")?;
            temp.write_all(bytes).context("write object bytes")?;
            temp.as_file().sync_all().context("sync object bytes")?;
            match temp.persist_noclobber(&path) {
                Ok(_) => {}
                Err(error) if path.exists() => {
                    // A concurrent writer stored the same content first, so the temporary file can be dropped.
                    drop(error.file);
                }
                Err(error) => return Err(error.error).context("persist object atomically"),
            }
        }

        Ok(StoredObject {
            hash: format!("sha256:{hash}"),
            size_bytes: bytes.len() as u64,
            relative_path: PathBuf::from(prefix).join(hash),
        })
    }

    /// Object identifiers accept only canonical hashes, preventing path traversal through user filenames.
    pub fn read(&self, object_hash: &str) -> Result<Vec<u8>> {
        let hash = object_hash
            .strip_prefix("sha256:")
            .context("object hash must use sha256 prefix")?;
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("invalid object hash");
        }
        let path = self.root.join(&hash[..2]).join(hash);
        let bytes = fs::read(&path).context("read stored object")?;
        let actual = format!("sha256:{:x}", Sha256::digest(&bytes));
        if actual != object_hash.to_ascii_lowercase() {
            bail!("stored object checksum mismatch");
        }
        Ok(bytes)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredObject {
    pub hash: String,
    pub size_bytes: u64,
    pub relative_path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_content_is_stored_once() {
        // Repeated uploads return the same immutable object identifier.
        let temp = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(temp.path()).unwrap();
        let first = store.put(b"course audio").unwrap();
        let second = store.put(b"course audio").unwrap();
        assert_eq!(first, second);
        assert_eq!(store.read(&first.hash).unwrap(), b"course audio");
    }

    #[test]
    fn traversal_like_identifier_is_rejected() {
        // User-controlled names never become filesystem paths.
        let temp = tempfile::tempdir().unwrap();
        let store = ObjectStore::new(temp.path()).unwrap();
        assert!(store.read("sha256:../../secret").is_err());
    }
}
