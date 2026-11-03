//! Native archive replacement under an OS-held lease. Never truncate the source.
use crate::{Result, StoreError, bytes_match_name, is_immutable, read_entries, sha256_hex};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
fn io(e: impl std::fmt::Display) -> StoreError {
    StoreError::Io(e.to_string())
}
pub struct LockedArchive {
    path: PathBuf,
    _lease: File,
}
pub fn lock(path: &Path) -> Result<LockedArchive> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io)?;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(StoreError::Unsupported("symlink archive target".into()));
    }
    let name = path
        .file_name()
        .ok_or_else(|| io("archive file name required"))?
        .to_string_lossy();
    let lease_path = parent.join(format!(".{name}.alto.lock"));
    if std::fs::symlink_metadata(&lease_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io("symlink archive lock"));
    }
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lease_path)
        .map_err(io)?;
    lease
        .try_lock()
        .map_err(|e| io(format!("BUSY: archive lease: {e}")))?;
    Ok(LockedArchive {
        path: path.to_path_buf(),
        _lease: lease,
    })
}
impl LockedArchive {
    pub fn read(&self) -> Result<Option<Vec<u8>>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(e)),
        }
    }
    /// `expected` is SHA-256 of the original bytes, or the empty string for a new file.
    pub fn replace(&self, bytes: &[u8], expected: &str) -> Result<()> {
        let current = self.read()?;
        let actual = current.as_ref().map(|b| sha256_hex(b)).unwrap_or_default();
        if actual != expected {
            return Err(io("CONFLICT: archive changed"));
        }
        let entries = read_entries(bytes)?;
        crate::decode_entries(entries.clone())?;
        for (name, data) in &entries {
            if is_immutable(name) && !bytes_match_name(name, data) {
                return Err(StoreError::Corrupt(format!("archive entry {name}")));
            }
        }
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let tmp = parent.join(format!(".alto-{}.tmp", alto_core::NodeId::new().0));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
                .map_err(io)?;
            file.write_all(bytes).map_err(io)?;
            file.sync_all().map_err(io)?;
            let check = std::fs::read(&tmp).map_err(io)?;
            if check != bytes {
                return Err(io("temporary archive verification failed"));
            }
            read_entries(&check)?;
            std::fs::rename(&tmp, &self.path).map_err(io)?;
            File::open(parent).and_then(|f| f.sync_all()).map_err(io)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_replace_and_invalid_zip_preserve_original() {
        let dir = std::env::temp_dir().join(format!("alto-atomic-{}", alto_core::NodeId::new().0));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.alto");
        let bytes = crate::encode_package(b"{}", Default::default()).unwrap();
        let target = lock(&path).unwrap();
        target.replace(&bytes, "").unwrap();
        assert!(lock(&path).is_err());
        assert!(target.replace(b"invalid", &sha256_hex(&bytes)).is_err());
        assert!(target.replace(&bytes, "stale").is_err());
        assert_eq!(target.read().unwrap().unwrap(), bytes);
        drop(target);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
