//! Per-session artifact partitions: one directory per session under the agent's artifacts tree.

use std::path::{Path, PathBuf};

const ARTIFACTS_DIR_NAME: &str = "session-artifacts";

/// The artifacts tree that owns a session dir. A session dir already inside the tree (an RLM
/// child's partition under `<root>/<parent>/<child>`) belongs to that tree; a top-level session
/// dir's tree is its sibling `session-artifacts` directory. The probe is bounded to the two
/// levels a partition can nest, so an unrelated directory deeper in the path never matches.
#[must_use]
pub fn session_artifacts_root(session_dir: &Path) -> Option<PathBuf> {
    for ancestor in session_dir.ancestors().skip(1).take(2) {
        if ancestor
            .file_name()
            .is_some_and(|name| name == ARTIFACTS_DIR_NAME)
        {
            return Some(ancestor.to_path_buf());
        }
    }
    Some(session_dir.parent()?.join(ARTIFACTS_DIR_NAME))
}

/// One session's artifact partition: the agent-level artifacts tree keyed by session id, so a
/// child never nests a second `session-artifacts` tree under its parent's partition.
#[must_use]
pub fn session_artifact_dir(session_dir: &Path, session_id: &str) -> Option<PathBuf> {
    Some(session_artifacts_root(session_dir)?.join(session_id))
}

/// A `session-artifacts` tree found inside another one, with the bytes its files occupy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedArtifactsRoot {
    pub path: PathBuf,
    pub bytes: u64,
}

/// Every `session-artifacts` root below `artifacts_root`, each sized over its whole subtree.
/// Read-only: nothing under the tree is created, renamed, or removed.
#[must_use]
pub fn nested_artifacts_roots(artifacts_root: &Path) -> Vec<NestedArtifactsRoot> {
    let mut nested = Vec::new();
    collect_nested_roots(artifacts_root, &mut nested);
    nested.sort_by(|left, right| left.path.cmp(&right.path));
    nested
}

fn collect_nested_roots(dir: &Path, nested: &mut Vec<NestedArtifactsRoot>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            continue;
        }
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name == ARTIFACTS_DIR_NAME)
        {
            nested.push(NestedArtifactsRoot {
                bytes: subtree_bytes(&path),
                path: path.clone(),
            });
        }
        collect_nested_roots(&path, nested);
    }
}

fn subtree_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total += subtree_bytes(&entry.path());
        } else if file_type.is_file() {
            total += entry.metadata().map_or(0, |metadata| metadata.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_session_partitions_beside_the_session_tree() {
        let dir = Path::new("/agent/sessions");
        assert_eq!(
            session_artifacts_root(dir),
            Some(PathBuf::from("/agent/session-artifacts"))
        );
        assert_eq!(
            session_artifact_dir(dir, "s1"),
            Some(PathBuf::from("/agent/session-artifacts/s1"))
        );
    }

    #[test]
    fn child_partitions_use_the_agents_artifacts_tree_not_a_nested_one() {
        let child_dir = Path::new("/agent/session-artifacts/parent/sub-1");
        assert_eq!(
            session_artifacts_root(child_dir),
            Some(PathBuf::from("/agent/session-artifacts"))
        );
        assert_eq!(
            session_artifact_dir(child_dir, "child-session"),
            Some(PathBuf::from("/agent/session-artifacts/child-session"))
        );
    }

    #[test]
    fn nested_roots_are_reported_with_the_bytes_below_them() {
        let agent = tempfile::tempdir().expect("agent dir");
        let nested = agent
            .path()
            .join("session-artifacts/parent/session-artifacts/child");
        std::fs::create_dir_all(nested.join("session-artifacts")).expect("nested tree");
        std::fs::write(nested.join("kernel-state.dill"), [0_u8; 8]).expect("file");
        std::fs::write(nested.join("session-artifacts/snapshot.dill"), [0_u8; 4]).expect("file");

        let roots = nested_artifacts_roots(&agent.path().join("session-artifacts"));
        assert_eq!(roots.len(), 2, "both nested roots: {roots:?}");
        assert_eq!(
            roots[0].path,
            nested.parent().expect("child tree"),
            "the shallow root sorts first"
        );
        assert_eq!(roots[1].path, nested.join("session-artifacts"));
        assert_eq!(roots[0].bytes, 12, "the shallow root counts its subtree");
        assert_eq!(roots[1].bytes, 4, "the deep root counts only its own files");
    }

    #[test]
    fn an_unrelated_ancestor_name_is_not_an_artifacts_tree() {
        let dir = Path::new("/srv/session-artifacts-archive/agent/sessions");
        assert_eq!(
            session_artifact_dir(dir, "s1"),
            Some(PathBuf::from(
                "/srv/session-artifacts-archive/agent/session-artifacts/s1"
            ))
        );
    }
}
