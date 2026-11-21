//! A session's on-disk storage: the conversation log, the artifact partition in the
//! agent's artifacts tree, the kernel snapshot inside that partition, and the info-cache
//! sidecar.

use std::fs;
use std::path::{Path, PathBuf};

use crate::kernel::{manifest_path_in, snapshot_path_in};

/// The byte footprint of one session's storage.
///
/// `artifact_directory` is the whole partition, kernel state included. The
/// kernel snapshot is reported separately because the delete surface names it
/// as its own step; subtracting it from the partition keeps the reported
/// figures additive instead of counting it twice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionStorageSizes {
    pub log: u64,
    pub artifact_directory: u64,
    pub kernel: u64,
    pub cache: u64,
}

impl SessionStorageSizes {
    /// The artifact partition without the kernel snapshot it contains.
    #[must_use]
    pub fn artifacts_excluding_kernel(&self) -> u64 {
        self.artifact_directory.saturating_sub(self.kernel)
    }

    /// Every byte the delete frees, with the kernel snapshot counted once.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.log
            .saturating_add(self.artifact_directory)
            .saturating_add(self.cache)
    }
}

/// The files a saved session owns: `<id>.jsonl`, `<id>.info-cache.json`, the agent's
/// `<root>/session-artifacts/<id>` partition, and the kernel snapshot files in that
/// partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStorage {
    log: PathBuf,
    artifacts: PathBuf,
    kernel_state: Vec<PathBuf>,
    cache: PathBuf,
}

impl SessionStorage {
    /// Resolve the storage for a conversation log, or `None` when the log has no
    /// id or no session directory to resolve the artifacts tree from.
    #[must_use]
    pub fn for_log(log: impl AsRef<Path>) -> Option<Self> {
        let log = log.as_ref();
        let id = log
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|id| !id.is_empty())?;
        let artifacts = crate::session::artifacts::session_artifact_dir(log.parent()?, id)?;
        let cache = log.with_file_name(format!("{id}.info-cache.json"));
        let kernel_state = vec![snapshot_path_in(&artifacts), manifest_path_in(&artifacts)];
        Some(Self {
            log: log.to_path_buf(),
            artifacts,
            kernel_state,
            cache,
        })
    }

    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log
    }

    #[must_use]
    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts
    }

    #[must_use]
    pub fn kernel_state_paths(&self) -> &[PathBuf] {
        &self.kernel_state
    }

    #[must_use]
    pub fn cache_path(&self) -> &Path {
        &self.cache
    }

    /// Measure the storage. Missing paths measure zero, and symbolic links are
    /// never followed: the delete removes the link, not its target.
    #[must_use]
    pub fn sizes(&self) -> SessionStorageSizes {
        let artifact_directory = directory_bytes(&self.artifacts);
        SessionStorageSizes {
            log: file_bytes(&self.log),
            artifact_directory,
            kernel: self.kernel_state.iter().map(|path| file_bytes(path)).sum(),
            cache: file_bytes(&self.cache),
        }
    }
}

fn file_bytes(path: &Path) -> u64 {
    fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map_or(0, |metadata| metadata.len())
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                return 0;
            };
            if metadata.is_dir() {
                directory_bytes(&path)
            } else if metadata.is_file() {
                metadata.len()
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a1fb6b-cd31-7694-a622-8cbd0fa54d8d";

    #[pa_test_scratch::pa_test]
    #[test]
    fn sizes_split_the_artifact_directory_from_its_kernel_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let sessions = root.path().join("sessions");
        let log = sessions.join(format!("{ID}.jsonl"));
        fs::create_dir_all(&sessions).unwrap();
        fs::write(&log, [0u8; 3]).unwrap();
        let artifacts = root.path().join("session-artifacts").join(ID);
        fs::create_dir_all(artifacts.join("sub")).unwrap();
        fs::write(artifacts.join("kernel-state.dill"), [0u8; 30]).unwrap();
        fs::write(artifacts.join("kernel-state.json"), [0u8; 5]).unwrap();
        fs::write(artifacts.join("data.bin"), [0u8; 100]).unwrap();
        fs::write(artifacts.join("sub").join("more.bin"), [0u8; 50]).unwrap();
        let cache = sessions.join(format!("{ID}.info-cache.json"));
        fs::write(&cache, [0u8; 7]).unwrap();

        let storage = SessionStorage::for_log(&log).expect("storage");
        assert_eq!(storage.log_path(), log.as_path());
        assert_eq!(storage.artifacts_dir(), artifacts.as_path());
        assert_eq!(storage.cache_path(), cache.as_path());
        assert_eq!(
            storage.sizes(),
            SessionStorageSizes {
                log: 3,
                artifact_directory: 185,
                kernel: 35,
                cache: 7,
            }
        );
        let sizes = storage.sizes();
        assert_eq!(sizes.artifacts_excluding_kernel(), 150);
        assert_eq!(sizes.total(), 195);
    }

    #[pa_test_scratch::pa_test]
    #[test]
    fn a_child_log_resolves_to_the_agents_artifacts_tree() {
        let root = tempfile::tempdir().unwrap();
        let child_dir = root
            .path()
            .join("session-artifacts")
            .join("parent")
            .join("child");
        fs::create_dir_all(&child_dir).unwrap();
        let log = child_dir.join(format!("{ID}.jsonl"));

        let storage = SessionStorage::for_log(&log).expect("storage");
        assert_eq!(
            storage.artifacts_dir(),
            root.path().join("session-artifacts").join(ID)
        );
    }

    #[pa_test_scratch::pa_test]
    #[test]
    fn absent_storage_measures_zero() {
        let root = tempfile::tempdir().unwrap();
        let log = root.path().join("sessions").join(format!("{ID}.jsonl"));
        let storage = SessionStorage::for_log(&log).expect("storage");
        assert_eq!(storage.sizes(), SessionStorageSizes::default());
    }
}
