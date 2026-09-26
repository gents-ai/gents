//! The one path a running agent takes to call an installed plugin.
//!
//! A model tool and a graph stage both call through here, so the lookup of
//! the installed record, the digest pin, the recorded grant and the budget
//! are decided in one place. An admitted runner is cached by artifact digest
//! and grant, so repeated calls skip the store read, the parse and the
//! admission check; a new install or a changed grant is admitted afresh.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};

use super::store::{self, InstalledPlugin};
use super::{Manifold, PluginBudget, PluginOutcome, PluginRunner};

/// Bytes of admitted artifacts kept in memory before the cache starts over.
// The whole cache is dropped past the budget; per-entry eviction is the
// upgrade if agents ever call more distinct plugins than fit.
const ADMITTED_BYTES_BUDGET: u64 = 512 * 1024 * 1024;

struct Admitted {
    granted: Option<Manifold>,
    runner: PluginRunner,
    budget: PluginBudget,
    bytes: u64,
}

/// One completed call: which artifact ran and what it returned.
#[derive(Debug, Clone)]
pub struct PluginCall {
    pub coordinate: String,
    /// `sha256:<hex>` of the artifact that ran.
    pub digest: String,
    pub outcome: PluginOutcome,
}

/// Calls installed plugins from one gents home.
pub struct PluginExecutor {
    home: Option<PathBuf>,
    admitted: kovan_map::HopscotchMap<String, Arc<Admitted>>,
    admitted_bytes: AtomicU64,
}

impl std::fmt::Debug for PluginExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginExecutor")
            .field("home", &self.home)
            .finish()
    }
}

impl Default for PluginExecutor {
    fn default() -> Self {
        Self::new(None)
    }
}

impl PluginExecutor {
    /// Plugins installed under `home`; `None` has none installed.
    pub fn new(home: Option<PathBuf>) -> Self {
        Self {
            home,
            admitted: kovan_map::HopscotchMap::new(),
            admitted_bytes: AtomicU64::new(0),
        }
    }

    /// The installed record for `coordinate`, which must still be the
    /// artifact `pinned` names when a pin is given.
    pub fn resolve(&self, coordinate: &str, pinned: Option<&str>) -> Result<InstalledPlugin> {
        let (namespace, name) = store::parse_coordinate(coordinate)?;
        let home = self.home.as_deref().with_context(|| {
            format!("plugin {coordinate} is not installed: this runtime has no plugin directory")
        })?;
        let record = store::read_record(home, namespace, name)
            .with_context(|| format!("plugin {coordinate} is not installed"))?;
        if let Some(pinned) = pinned {
            anyhow::ensure!(
                record.digest == pinned,
                "plugin {coordinate} is installed as {}, not the pinned {pinned}; reinstall it or update the pin",
                record.digest
            );
        }
        Ok(record)
    }

    /// Runs `record`'s artifact once on `input`, within its recorded grant.
    ///
    /// The call runs on the blocking pool. Dropping the future does not stop
    /// a call already running; the plugin's own wall-clock budget does.
    pub async fn call(
        &self,
        record: &InstalledPlugin,
        input: serde_json::Value,
    ) -> Result<PluginCall> {
        let admitted = self.admit(record)?;
        let coordinate = format!("{}/{}", record.namespace, record.name);
        let outcome =
            tokio::task::spawn_blocking(move || admitted.runner.call(&input, &admitted.budget))
                .await
                .with_context(|| format!("plugin {coordinate} stopped unexpectedly"))??;
        Ok(PluginCall {
            coordinate,
            digest: record.digest.clone(),
            outcome,
        })
    }

    fn admit(&self, record: &InstalledPlugin) -> Result<Arc<Admitted>> {
        if let Some(admitted) = self.admitted.get(&record.digest) {
            if admitted.granted == record.granted {
                return Ok(admitted);
            }
        }
        let home = self
            .home
            .as_deref()
            .context("this runtime has no plugin directory")?;
        let digest_hex = record.digest.strip_prefix("sha256:").with_context(|| {
            format!("installed plugin digest {:?} is not sha256", record.digest)
        })?;
        let bytes = store::read_bytes(home, digest_hex)?;
        let afb = afterburner_afb::Afb::from_bytes(&bytes).map_err(|error| {
            anyhow::anyhow!(
                "installed plugin {}/{} is not a readable .afb: {error}",
                record.namespace,
                record.name
            )
        })?;
        let budget = PluginBudget::for_artifact(&afb);
        let runner = PluginRunner::compile_within(&bytes, &record.declaration, &record.ceiling())?;
        let admitted = Arc::new(Admitted {
            granted: record.granted.clone(),
            runner,
            budget,
            bytes: bytes.len() as u64,
        });
        let total = self
            .admitted_bytes
            .fetch_add(admitted.bytes, Ordering::Relaxed)
            + admitted.bytes;
        if total > ADMITTED_BYTES_BUDGET {
            self.admitted.clear();
            self.admitted_bytes.store(admitted.bytes, Ordering::Relaxed);
        }
        if let Some(previous) = self
            .admitted
            .insert(record.digest.clone(), admitted.clone())
        {
            self.admitted_bytes
                .fetch_sub(previous.bytes, Ordering::Relaxed);
        }
        Ok(admitted)
    }

    #[cfg(test)]
    pub(crate) fn admitted_len(&self) -> usize {
        self.admitted.len()
    }
}
