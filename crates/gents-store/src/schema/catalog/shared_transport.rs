use defra_node::Node;
use gents_protocol::schemas::{identity_policy_resource, BuiltInPolicy, HomePolicyIds};

use super::{BaselineCollection, SchemaError};

/// The account and object IDs selected during household admission. Installation
/// verifies these against Vera before accepting an existing collection owner.
pub(crate) struct SharedTransportHome {
    pub account: identity::Did,
    pub group: String,
    pub host_node: String,
    pub device_node: String,
}

/// Install only the collections needed to create signed device registrations
/// and their group before the account can register transport collections.
pub(crate) async fn ensure_admission_schema(node: &Node) -> Result<(), SchemaError> {
    use gents_protocol::schemas::{ACCOUNT_DEVICE_GROUP_NAME, NODE_NAME};

    let ids = crate::local::document_acp::node_policy_ids(node);
    let catalog = super::runtime_catalog(ids);
    let entries = [NODE_NAME, ACCOUNT_DEVICE_GROUP_NAME].map(|name| {
        *catalog
            .baseline
            .iter()
            .find(|entry| entry.name == name)
            .expect("device admission collections are in the bundled catalog")
    });
    super::ensure(
        node,
        &super::Catalog {
            baseline: &entries,
            ordinary_indexes: &[],
        },
    )
    .await
}

pub(super) fn resource(ids: &HomePolicyIds, collection: &str) -> Option<&'static str> {
    if ids != HomePolicyIds::embedded_vera_transport_v2()
        || !gents_protocol::schemas::is_replicated_protocol_collection(collection)
    {
        return None;
    }
    identity_policy_resource(collection).filter(|resource| {
        matches!(
            *resource,
            "discovery" | "scoped_discovery" | "enrollment_records"
        )
    })
}

pub(super) async fn verify(
    node: &Node,
    entry: &BaselineCollection<'_>,
    resource: &str,
) -> Result<acp::TransportSentinelQuery, SchemaError> {
    let refused = || SchemaError::SharedTransportAdmission {
        collection: entry.name.to_owned(),
    };
    let scope =
        crate::node_extension::attached::<super::SharedTransportHome>(node).ok_or_else(refused)?;
    let selection = acp::TransportSentinelQuery {
        policy: HomePolicyIds::embedded_vera_transport_v2()
            .id(BuiltInPolicy::Household)
            .into(),
        resource: resource.into(),
        child: entry.expected_version.ok_or_else(refused)?.into(),
        parent: scope.host_node.clone(),
        device_node: scope.device_node.clone(),
        group: scope.group.clone(),
        account: scope.account.to_string(),
        device: node
            .node_identity_did()
            .ok_or(SchemaError::MissingNodeIdentity)?
            .into(),
    };
    if !node
        .document_acp()
        .verify_transport_sentinel(&selection)
        .await
        .map_err(defra_node::Error::from)?
    {
        return Err(refused());
    }
    Ok(selection)
}
