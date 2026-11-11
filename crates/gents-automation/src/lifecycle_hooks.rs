use std::sync::{Arc, OnceLock};

use gents_session::request::hooks::registry::Registry;

/// The bundled lifecycle plugins, in their fixed order: triggers (core), then
/// Goals, with the core operations and reads their effects may name. Bundled
/// plugins are always on, so a plugin missing from this list is a defect of
/// the build, not a state a node can be in. Every listener is registered from
/// this crate or below it, so its tests compose the same registry the runtime
/// does.
pub fn bundled() -> Arc<Registry> {
    static BUNDLED: OnceLock<Arc<Registry>> = OnceLock::new();
    Arc::clone(BUNDLED.get_or_init(|| {
        let mut registry = Registry::default();
        gents_session::request::lifecycle::requested_create::register(&mut registry);
        gents_session::request::hooks::operations::register(&mut registry);
        gents_tools::native::register_lifecycle_reads(&mut registry);
        crate::triggers::lifecycle_hooks::register(&mut registry);
        crate::goal::lifecycle_hooks::register(&mut registry);
        Arc::new(registry)
    }))
}

/// Attaches the bundled plugins to `node`. The composition root calls this
/// for every node it starts; a test calls it for a node it built directly
/// when it asserts behaviour the plugins own.
pub fn attach(
    node: &Arc<defra_node::Node>,
) -> Result<(), gents_store::node_extension::NodeExtensionError> {
    Registry::attach(node, bundled())
}
