//! One initialized store per test binary, copied into each fresh test home.
//!
//! Initializing a protected store starts a node, applies every runtime schema
//! and the access policy, and stops it: 0.7 s alone and several seconds on a
//! loaded CI host, paid by nearly every test. Copying the closed store costs
//! milliseconds. The Nth fresh home a process creates is a copy of template N,
//! so the nodes one test starts keep distinct node, owner and store keys, as
//! fresh initialization gave them; homes in different processes share keys
//! and never meet. A node that needs an identity of its own across processes
//! (a P2P peer, a held node key) is initialized fresh instead.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};

/// The home entries an initialized store consists of. The store lock
/// (`data.lock`) lives beside `data` and is never copied.
const ENTRIES: &[&str] = &["node.key", "owner.key", "store.key", "data"];

static FRESH_HOMES: AtomicUsize = AtomicUsize::new(0);

/// Copies the next of this binary's template stores into `home`, building it
/// on its first use in any process of this binary.
pub(super) async fn copy_into(home: &Path) -> Result<()> {
    let template = template(FRESH_HOMES.fetch_add(1, Ordering::Relaxed)).await?;
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&home)?;
        for entry in ENTRIES {
            copy_entry(&template.join(entry), &home.join(entry))
                .with_context(|| format!("copying the store template's {entry}"))?;
        }
        Ok(())
    })
    .await?
}

/// Template `index` of this test binary: keyed by the binary's name, size and
/// modification time, since the binary carries the schemas and policy it
/// applies. Processes that race to build it each initialize a staging home and
/// publish it by renaming it into place; a rename onto an existing template
/// fails and the loser's staging home is removed.
async fn template(index: usize) -> Result<PathBuf> {
    let root = std::env::temp_dir()
        .join("gents-store-templates")
        .join(binary_key()?);
    let dir = root.join(index.to_string());
    if tokio::fs::try_exists(&dir).await? {
        return Ok(dir);
    }
    tokio::fs::create_dir_all(&root).await?;
    let staging_root = root.clone();
    let staging = tokio::task::spawn_blocking(move || {
        tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(&staging_root)
    })
    .await??;
    drop(super::embedded::open_or_initialize_home(staging.path()).await?);
    let staged = staging.keep();
    match tokio::fs::rename(&staged, &dir).await {
        Ok(()) => Ok(dir),
        Err(_) if tokio::fs::try_exists(&dir).await? => {
            tokio::fs::remove_dir_all(&staged).await?;
            Ok(dir)
        }
        Err(error) => Err(error).context("publishing the store template"),
    }
}

fn binary_key() -> Result<String> {
    let binary = std::env::current_exe()?;
    let metadata = std::fs::metadata(&binary)?;
    let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
    let name = binary
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("test");
    Ok(format!("{name}-{}-{modified}", metadata.len()))
}

fn copy_entry(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_entry(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}
