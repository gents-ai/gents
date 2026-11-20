/// P2P administration must carry the node DID explicitly across worker boundaries.
pub fn operations(
    node: &defra_node::Node,
) -> Option<std::sync::Arc<dyn defra_p2p_adapter::P2POperations>> {
    let context = defra_core::RequestContext::default()
        .with_identity(node.node_identity_did().map(ToOwned::to_owned));
    node.p2p_arc().and_then(|p2p| p2p.bind_request(&context))
}
