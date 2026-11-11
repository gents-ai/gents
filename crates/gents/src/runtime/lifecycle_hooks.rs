use std::sync::{Arc, OnceLock};

use gents_session::request::hooks::registry::Registry;

/// The bundled lifecycle plugins, in their fixed order: triggers (core), then
/// Goals, with the core operations and reads their effects may name. Bundled
/// plugins are always on, so a plugin missing from this list is a defect of
/// the build, not a state a node can be in.
pub fn bundled() -> Arc<Registry> {
    static BUNDLED: OnceLock<Arc<Registry>> = OnceLock::new();
    Arc::clone(BUNDLED.get_or_init(|| {
        let mut registry = Registry::default();
        gents_session::request::lifecycle::requested_create::register(&mut registry);
        gents_session::request::hooks::operations::register(&mut registry);
        gents_tools::native::register_lifecycle_reads(&mut registry);
        gents_automation::triggers::lifecycle_hooks::register(&mut registry);
        gents_automation::goal::lifecycle_hooks::register(&mut registry);
        Arc::new(registry)
    }))
}

/// Installs the bundled plugins for this process. Every production node start
/// goes through here, so a served node always runs its lifecycle points with
/// them.
pub fn install() {
    Registry::install(bundled());
}
