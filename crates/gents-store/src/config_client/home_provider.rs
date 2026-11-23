//! Where the home a caller works against decides document access, and so
//! which IDs its built-in policies have (`DocumentAcpChoice.policyIds`).

use gents_protocol::client_authentication::ClientAuthentication;
use gents_protocol::schemas::HomePolicyIds;

use super::{ConfigAccess, ConfigClientError, GraphqlEndpoint, Result};
use crate::local::document_acp::{home_policy_ids, provider_of, DocumentAcp};

impl GraphqlEndpoint {
    /// Bind the setup endpoint to the schema selected by its local node.
    pub fn with_local_home_schema(
        self,
        node: &defra_node::Node,
    ) -> std::result::Result<Self, crate::node_extension::ExtensionError> {
        if let Some(bound) =
            crate::node_extension::attached::<crate::local::document_acp::BoundPolicyIds>(node)
        {
            self.with_extension(bound)
        } else {
            Ok(self)
        }
    }

    /// The provider of the runtime this endpoint reaches: as its attested
    /// status reports how clients authenticate to it, or, for an endpoint
    /// opened with a fixed authentication, as the delegation audience its
    /// home reported. Only a home on a Vera node names one
    /// (`document_acp::status_reports`).
    pub(crate) async fn home_provider(&self) -> Result<DocumentAcp> {
        match self.authenticated_provider().await? {
            DocumentAcp::Local if self.delegation_audience.get().is_some() => Ok(DocumentAcp::Vera),
            provider => Ok(provider),
        }
    }

    /// The provider as the runtime says it authenticates this endpoint's
    /// client alone, which an endpoint opened with a fixed authentication
    /// reports as the node's own store whatever its home.
    pub(crate) async fn authenticated_provider(&self) -> Result<DocumentAcp> {
        match self.runtime_authentication().await? {
            ClientAuthentication::Local => Ok(DocumentAcp::Local),
            ClientAuthentication::Vera { .. } => Ok(DocumentAcp::Vera),
            ClientAuthentication::Unavailable => {
                Err(ConfigClientError::RuntimeAuthenticationUnavailable)
            }
        }
    }

    /// The IDs the built-in policies have in the home this endpoint reaches.
    pub async fn home_policy_ids(&self) -> Result<&'static HomePolicyIds> {
        if matches!(
            self.runtime_authentication().await?,
            ClientAuthentication::Vera {
                shared_household: true,
                ..
            }
        ) {
            return Ok(HomePolicyIds::embedded_vera_transport_v2());
        }
        if let Some(ids) = self
            .extensions
            .get::<crate::local::document_acp::BoundPolicyIds>()
        {
            return Ok(ids.ids());
        }
        Ok(home_policy_ids(self.home_provider().await?))
    }
}

impl ConfigAccess {
    /// The provider of the home this access works against.
    pub async fn home_provider(&self) -> Result<DocumentAcp> {
        match self {
            Self::Graphql(endpoint) => endpoint.home_provider().await,
            Self::Local(local) => Ok(provider_of(local.node())),
        }
    }

    /// The IDs the built-in policies have in the home this access works
    /// against.
    pub async fn home_policy_ids(&self) -> Result<&'static HomePolicyIds> {
        match self {
            Self::Graphql(endpoint) => endpoint.home_policy_ids().await,
            Self::Local(local) => Ok(crate::local::document_acp::node_policy_ids(local.node())),
        }
    }
}
