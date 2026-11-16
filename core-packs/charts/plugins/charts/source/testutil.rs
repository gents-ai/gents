//! Helpers shared by the tests: a scratch folder that removes itself.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A fresh empty folder under the system temporary folder.
pub struct TempDir(PathBuf);

impl TempDir {
    /// Creates the folder.
    pub fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("charts-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Self(dir)
    }

    /// The folder.
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Writes `bytes` to `name` below the folder, creating folders on the way.
    pub fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(&p, bytes).expect("write");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
