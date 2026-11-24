//! Output SSTables a compaction has created but not yet offered to the
//! manifest.
//!
//! A compaction allocates and writes its outputs before it installs them.
//! If it fails in between, nothing references those files: the manifest
//! never learns of them, and no later pass deletes them. A compaction that
//! keeps failing the same way (out of file descriptors, say) then leaks a
//! full set of outputs on every retry until the disk fills.
//!
//! [`PendingOutputs`] owns the paths from the moment each file is
//! allocated. Dropping it unlinks every one of them, so any early return
//! cleans up. [`PendingOutputs::offered_to_manifest`] gives up that claim
//! just before the version edit is applied. From then on the manifest may
//! reference the files even if `apply` reports an error, so only a sweep
//! against the replayed manifest can safely decide what is unreferenced.

use std::path::PathBuf;
use std::sync::Arc;

use crate::env::Env;

pub(crate) struct PendingOutputs {
    env: Arc<dyn Env>,
    paths: Vec<PathBuf>,
}

impl PendingOutputs {
    pub(crate) fn new(env: Arc<dyn Env>) -> Self {
        Self {
            env,
            paths: Vec::new(),
        }
    }

    /// Claim a path before anything is written to it, so a failure while
    /// creating the file is cleaned up too.
    pub(crate) fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    /// Move every claimed path into a new owner, leaving this one empty.
    pub(crate) fn take(&mut self) -> Self {
        Self {
            env: Arc::clone(&self.env),
            paths: std::mem::take(&mut self.paths),
        }
    }

    /// Give up the claim. Call this immediately before the manifest edit
    /// that names these files is applied, whatever that apply returns.
    pub(crate) fn offered_to_manifest(mut self) {
        self.paths.clear();
    }
}

impl Drop for PendingOutputs {
    fn drop(&mut self) {
        for path in self.paths.drain(..) {
            match self.env.remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not unlink an output of a failed compaction"
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::std_env as default_env;

    fn touch(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        path
    }

    #[test]
    fn dropping_unlinks_every_tracked_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "000001.sst");
        let b = touch(dir.path(), "000002.sst");
        let mut pending = PendingOutputs::new(default_env());
        pending.track(a.clone());
        pending.track(b.clone());
        drop(pending);
        assert!(!a.exists());
        assert!(!b.exists());
    }

    #[test]
    fn a_path_that_was_never_created_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut pending = PendingOutputs::new(default_env());
        pending.track(dir.path().join("000009.sst"));
        drop(pending);
    }

    #[test]
    fn offering_to_the_manifest_keeps_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "000001.sst");
        let mut pending = PendingOutputs::new(default_env());
        pending.track(a.clone());
        pending.offered_to_manifest();
        assert!(a.exists());
    }

    #[test]
    fn take_moves_the_claim_to_the_new_owner() {
        let dir = tempfile::tempdir().unwrap();
        let a = touch(dir.path(), "000001.sst");
        let mut writer_side = PendingOutputs::new(default_env());
        writer_side.track(a.clone());
        let caller_side = writer_side.take();
        drop(writer_side);
        assert!(a.exists(), "the emptied owner must not unlink anything");
        drop(caller_side);
        assert!(!a.exists());
    }
}
