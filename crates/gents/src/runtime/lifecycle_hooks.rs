use std::sync::Arc;

use gents_session::request::hooks::registry::Registry;
use gents_store::config_client::GraphqlEndpoint;
use gents_store::node_extension::ExtensionError;

pub fn compose(node: &Arc<defra_node::Node>) -> Result<(), ExtensionError> {
    gents_automation::lifecycle_hooks::attach(node)
}

pub fn compose_endpoint(endpoint: GraphqlEndpoint) -> Result<GraphqlEndpoint, ExtensionError> {
    Registry::carried_by(endpoint, gents_automation::lifecycle_hooks::bundled())
}
