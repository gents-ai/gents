use gents_store::config_client::GraphqlEndpoint;
use gents_store::node_extension::ExtensionError;

pub fn compose_endpoint(endpoint: GraphqlEndpoint) -> Result<GraphqlEndpoint, ExtensionError> {
    endpoint.with_extensions(&gents_automation::lifecycle_hooks::composition())
}
