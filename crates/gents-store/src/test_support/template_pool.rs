use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Slot<M> {
    dir: tempfile::TempDir,
    metadata: M,
    in_use: AtomicBool,
}

/// A live checkout from a [`TemplateHomePool`]. Releasing it returns the
/// template slot to the idle pool so no two concurrently live fixtures share
/// the same template identity.
pub struct TemplateLease {
    release: Box<dyn FnOnce() + Send + Sync>,
}

impl std::fmt::Debug for TemplateLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TemplateLease")
    }
}

impl Drop for TemplateLease {
    fn drop(&mut self) {
        let release = std::mem::replace(&mut self.release, Box::new(|| {}));
        release();
    }
}

/// Process-local pool of cleanly shut-down template home directories.
///
/// Each slot holds its own distinct node identity and encrypted Regolith store
/// created through the production owner. Concurrent callers check out distinct
/// slots so concurrently live fixtures never share a node DID; dropped fixtures
/// return their slot for later reuse over a fresh temporary copy.
pub struct TemplateHomePool<M> {
    slots: Mutex<Vec<Arc<Slot<M>>>>,
}

impl<M> Default for TemplateHomePool<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> TemplateHomePool<M> {
    pub const fn new() -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
        }
    }
}

impl<M> TemplateHomePool<M>
where
    M: Clone + Send + Sync + 'static,
{
    pub async fn checkout<F, Fut>(&self, create: F) -> (TemplateLease, PathBuf, M)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = (tempfile::TempDir, M)>,
    {
        if let Some(slot) = self.try_claim_idle() {
            let path = slot.dir.path().to_path_buf();
            let metadata = slot.metadata.clone();
            return (lease_for(slot), path, metadata);
        }
        let (dir, metadata) = create().await;
        let slot = Arc::new(Slot {
            dir,
            metadata,
            in_use: AtomicBool::new(true),
        });
        let path = slot.dir.path().to_path_buf();
        let metadata = slot.metadata.clone();
        self.slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::clone(&slot));
        (lease_for(slot), path, metadata)
    }

    fn try_claim_idle(&self) -> Option<Arc<Slot<M>>> {
        let slots = self
            .slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        slots.iter().find_map(|slot| {
            slot.in_use
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .ok()
                .map(|_| Arc::clone(slot))
        })
    }
}

fn lease_for<M>(slot: Arc<Slot<M>>) -> TemplateLease
where
    M: Send + Sync + 'static,
{
    TemplateLease {
        release: Box::new(move || {
            slot.in_use.store(false, Ordering::Release);
        }),
    }
}

/// Copies a cleanly shut-down template home directory into `dst`, skipping
/// lock files and preserving private key file modes.
pub fn clone_template_home(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_dir(src, dst)
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "data.lock" {
            continue;
        }
        let source_path = entry.path();
        let target_path = dst.join(&name);
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir(&source_path, &target_path)?;
        } else if kind.is_file() {
            std::fs::copy(&source_path, &target_path)?;
            #[cfg(unix)]
            if source_path.extension().is_some_and(|ext| ext == "key") {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&target_path, std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    Ok(())
}
