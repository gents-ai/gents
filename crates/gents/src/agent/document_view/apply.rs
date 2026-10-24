use super::{ControlUpdateOutcome, DocumentRuntimeView, DocumentViewError};
use crate::collection::Collection;

/// Runtime-observed collections that are not themselves configuration.
/// PackInstallation is not itself configuration: a pack install or remove
/// writes or deletes only this record, and the plugin-store change it carries
/// reaches change detection through the resolved plugin identities on the
/// tool surface, so it must still wake the reconciler.
const OBSERVED_NON_CONFIG_COLLECTIONS: [&str; 5] = [
    "OAuthCredential",
    "ToolServiceHealthState",
    "GraphRevision",
    "GraphRun",
    "PackInstallation",
];

/// Every collection whose change can alter the runtime control snapshot.
pub(crate) fn control_collection_names() -> impl Iterator<Item = &'static str> {
    Collection::ALL
        .iter()
        .map(|collection| collection.graphql_type())
        .chain(OBSERVED_NON_CONFIG_COLLECTIONS)
}

pub(crate) async fn apply_control_update(
    node_did: &str,
    collection_name: &str,
    _doc_id: &str,
    view: &mut DocumentRuntimeView,
) -> Result<ControlUpdateOutcome, DocumentViewError> {
    if view.node.value.node_did != node_did {
        return Err(DocumentViewError::OwnerMismatch);
    }
    if control_collection_names().any(|name| name == collection_name) {
        Ok(ControlUpdateOutcome::FullReload)
    } else {
        Ok(ControlUpdateOutcome::Irrelevant)
    }
}
