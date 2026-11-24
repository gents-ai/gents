use defra_node::Node;
use gents_protocol::schemas::{identity_policy_resource, BuiltInPolicy, HomePolicyIds};

use super::{BaselineCollection, SchemaError};

/// The account and object IDs selected during household admission. Installation
/// verifies these against Vera before accepting an existing collection owner.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SharedTransportHome {
    pub account: identity::Did,
    pub group: String,
    pub host_node: String,
    pub device_node: String,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HostAdmissionError {
    #[error("missing host identity")]
    MissingIdentity,
    #[error("host has no verified delegation audience")]
    MissingAudience,
    #[error(transparent)]
    Identity(#[from] identity::Error),
    #[error(transparent)]
    Endpoint(#[from] crate::node_extension::ExtensionError),
    #[error(fmt = gents_protocol::error_chain::held)]
    Config(#[from] crate::config_client::ConfigClientError),
    #[error("transport registration failed: {0}")]
    Registration(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl SharedTransportHome {
    pub(crate) async fn create_host(
        node: &std::sync::Arc<Node>,
        url: String,
        signer: crate::config_client::ClientSigningCapability,
        account: &identity::Did,
    ) -> Result<Self, HostAdmissionError> {
        use crate::config_client::{transport_sentinel, ConfigAccess, GraphqlEndpoint};
        use crate::local::document_acp::{self, DocumentAcp};

        let device = identity::Did::new(
            node.node_identity_did()
                .ok_or(HostAdmissionError::MissingIdentity)?,
        )?;
        let endpoint = GraphqlEndpoint::as_principal(url, account.as_str())
            .with_client_signing_capability(signer.clone())?
            .with_local_home_schema(node)?
            .with_client_signed_agent_creates()
            .with_reported_delegation_audience(document_acp::status_reports(
                DocumentAcp::Vera,
                document_acp::node_delegation_audience(node),
            ))?;
        let access = ConfigAccess::Graphql(endpoint);
        let group = access.create_device_group(account, &device).await?;
        let host_node = access
            .set_shared_device_membership(&group, &device, true)
            .await?;
        let audience = access
            .request_bound_audience()
            .await?
            .ok_or(HostAdmissionError::MissingAudience)?;
        let approval =
            transport_sentinel::prepare_publisher(&signer, &audience, &host_node, account)
                .map_err(|error| HostAdmissionError::Registration(error.into()))?;
        transport_sentinel::submit_publisher(node, account.as_str(), approval)
            .await
            .map_err(|error| HostAdmissionError::Registration(error.into()))?;
        let ids = HomePolicyIds::embedded_vera_transport_v2();
        for entry in super::runtime_catalog(ids).baseline {
            if resource(ids, entry.name).is_some() {
                let audience = access
                    .request_bound_audience()
                    .await?
                    .ok_or(HostAdmissionError::MissingAudience)?;
                let approval =
                    transport_sentinel::prepare(&signer, &audience, entry.name, &host_node)
                        .map_err(|error| HostAdmissionError::Registration(error.into()))?;
                transport_sentinel::submit(node, account.as_str(), approval)
                    .await
                    .map_err(|error| HostAdmissionError::Registration(error.into()))?;
            }
        }
        Ok(Self {
            account: account.clone(),
            group,
            device_node: host_node.clone(),
            host_node,
        })
    }
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

/// Recheck every installed collection against its certified registration before
/// serving a reopened shared home. This path must not recreate a withdrawn grant.
pub(crate) async fn verify_installed_transport(node: &Node) -> Result<(), SchemaError> {
    let ids = crate::local::document_acp::node_policy_ids(node);
    for entry in super::runtime_catalog(ids).baseline {
        let Some(resource) = resource(ids, entry.name) else {
            continue;
        };
        let proof = verify(node, entry, resource).await?;
        let installed = node.get_collection(entry.name)?;
        if !installed.is_some_and(|collection| collection.collection_id == proof.child) {
            return Err(SchemaError::SharedTransportAdmission {
                collection: entry.name.to_owned(),
            });
        }
    }
    Ok(())
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
