//! The origin of a job and the engine guard.
use std::path::Path;

use gents_protocol::job::JobOrigin;

use crate::pack::home_install::recorded_provenance;
use crate::pack::{read_home_install, InstalledVia, PackKind};
use crate::plugin::store::InstalledPlugin;
use crate::tools::background::job::admit::Refusal;

/// Lean `callOrigin`: a call with a non-empty `script` is `dynamic`; any other
/// call is `installed`.
pub fn call_origin(script: Option<&str>) -> JobOrigin {
    if script.is_some_and(|script| !script.is_empty()) {
        JobOrigin::Dynamic
    } else {
        JobOrigin::Installed
    }
}

/// The engine guard: `plugin`'s owning pack must read `Operator` under
/// [`recorded_provenance`]. An unreadable record fails closed.
pub fn engine_guard(home: &Path, plugin: &InstalledPlugin) -> Result<(), Refusal> {
    let record = plugin
        .owner_pack_coordinate
        .as_deref()
        .and_then(|pack| read_home_install(home, pack).ok().flatten());
    let kind = record
        .as_ref()
        .map_or(PackKind::Documents, |record| record.kind.clone());
    let via = recorded_provenance(kind, record.map(|record| record.installed_via));
    if via == InstalledVia::Operator {
        Ok(())
    } else {
        Err(Refusal::NotAvailable)
    }
}

/// The call that follows an [`engine_guard`] refusal.
pub fn engine_guard_next(plugin: &InstalledPlugin) -> String {
    let pack = plugin
        .owner_pack_coordinate
        .clone()
        .unwrap_or_else(|| format!("{}/{}", plugin.namespace, plugin.name));
    format!("Next: the operator reinstalls {pack} with `gents pack install`")
}
