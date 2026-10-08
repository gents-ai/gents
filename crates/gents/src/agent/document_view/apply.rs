use super::{ControlUpdateOutcome, DocumentRuntimeView};
use crate::collection::Collection;
use anyhow::Result;
use defra_node::EmbeddedNode;

/// Configuration is one candidate snapshot, not a sequence of per-row grants.
/// Reload through the shared owner on any canonical config change; the watcher
/// resolves and publishes the complete candidate through its existing gate.
/// This also handles deletes and ownership moves without retaining a stale row.
pub(crate) async fn apply_control_update(
    node: &EmbeddedNode,
    agent_did: &str,
    collection_name: &str,
    _doc_id: &str,
    view: &mut DocumentRuntimeView,
) -> Result<ControlUpdateOutcome> {
    anyhow::ensure!(
        view.principal.value.agent_did == agent_did,
        "runtime view owner mismatch"
    );
    if Collection::ALL
        .iter()
        .any(|c| c.graphql_type() == collection_name)
        // PackInstallation is not itself configuration: a pack install or
        // remove writes or deletes only this record, and the plugin-store
        // change it carries reaches change detection through the resolved
        // plugin identities on the tool surface, so it must still wake the
        // reconciler.
        || matches!(
            collection_name,
            "OAuthCredential" | "ToolServiceHealthState" | "GraphRevision" | "GraphRun"
                | "PackInstallation"
        )
    {
        Ok(ControlUpdateOutcome::FullReload)
    } else if node.get_collection(collection_name)?.is_none() {
        // Watchers may carry a physical collection ID before name resolution.
        // An unresolved identifier cannot justify dropping the notification.
        Ok(ControlUpdateOutcome::FullReload)
    } else {
        Ok(ControlUpdateOutcome::Irrelevant)
    }
}
