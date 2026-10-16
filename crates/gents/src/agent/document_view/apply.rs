use super::{ControlUpdateOutcome, DocumentRuntimeView};
use crate::collection::Collection;
use anyhow::Result;
use defra_node::EmbeddedNode;

pub(crate) async fn apply_control_update(
    node: &EmbeddedNode,
    node_did: &str,
    collection_name: &str,
    _doc_id: &str,
    view: &mut DocumentRuntimeView,
) -> Result<ControlUpdateOutcome> {
    anyhow::ensure!(
        view.node.value.node_did == node_did,
        "runtime view owner mismatch"
    );
    if Collection::ALL
        .iter()
        .any(|c| c.graphql_type() == collection_name)
        // PackInstallation changes only this record, but the plugin-store
        // change it carries reaches change detection only through the
        // reconciler wake.
        || matches!(
            collection_name,
            "OAuthCredential" | "ToolServiceHealthState" | "GraphRevision" | "GraphRun"
                | "PackInstallation"
        )
    {
        Ok(ControlUpdateOutcome::FullReload)
    } else if node.get_collection(collection_name)?.is_none() {
        // Watchers may carry a physical collection ID before name resolution;
        // do not drop the notification.
        Ok(ControlUpdateOutcome::FullReload)
    } else {
        Ok(ControlUpdateOutcome::Irrelevant)
    }
}
