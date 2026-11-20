use defra_node::Node as EmbeddedNode;
use gents_store::local::document_acp::{node_periodic, Periodic, PeriodicLoop};
use tokio_util::sync::CancellationToken;

/// Whether `publication` stays idle on `node` (`Identity.NodePublication.periodic`):
/// when it does, this says so once and returns only once `cancel` fires, and
/// the caller writes nothing.
pub(crate) async fn idles(
    node: &EmbeddedNode,
    publication: Periodic,
    cancel: &CancellationToken,
) -> bool {
    if node_periodic(node, publication) == PeriodicLoop::Beats {
        return false;
    }
    tracing::info!(
        ?publication,
        "peer publication waits for the home's provider to grant transport relationships"
    );
    cancel.cancelled().await;
    true
}
