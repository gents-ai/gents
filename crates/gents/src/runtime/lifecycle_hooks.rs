use std::sync::Arc;

use gents_session::request::hooks::registry::Registry;
use gents_store::node_extension::NodeExtensionError;

/// Composes a started node with the bundled lifecycle plugins. Every
/// production node start goes through here, so a served node always runs its
/// lifecycle points with them; this process's transactions over a GraphQL
/// endpoint run with the same plugins.
pub fn compose(node: &Arc<defra_node::Node>) -> Result<(), NodeExtensionError> {
    let bundled = gents_automation::lifecycle_hooks::bundled();
    Registry::install_for_remote_transactions(Arc::clone(&bundled));
    Registry::attach(node, bundled)
}
