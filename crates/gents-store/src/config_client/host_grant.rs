//! An Agent's `host` grant sent under a request-bound delegation of its
//! owner, so that the grant resent under the same delegation takes effect once
//! (`Identity.ClientAgentJournal.resumeAttempt`). DefraDB's relationship route
//! authenticates its caller by a plain delegation only, so the runtime serves
//! this grant on a route of its own and sends it to its provider bound to the
//! delegation the request carries.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::stored_create::{StoredCreateCommands, HOST_RELATION};
use super::{graphql, graphql_api_base, ConfigAccess, ConfigClientError};
use crate::local::document_acp::{home_policy_ids, provider_of, DocumentAcp};

pub(super) const HOST_GRANT_PATH: &str = "/gents/agent/host";

/// A request-bound delegation for one Agent's `host` grant and the provider
/// time it lapses at.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostCredential {
    token: String,
    expires_at: u64,
}

impl HostCredential {
    /// Whether the provider still takes it at its time `now`, in Unix seconds.
    pub fn holds_at(&self, now: u64) -> bool {
        now < self.expires_at
    }

    /// This credential as the provider sees it once its lifetime ended.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[doc(hidden)]
    pub fn lapsed_for_test(self) -> Self {
        Self {
            expires_at: 0,
            ..self
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HostGrant {
    pub(super) agent_doc: String,
    pub(super) node: String,
    pub(super) token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HostGrantError {
    #[error(
        "this home decides document access in its own store and takes no request-bound host grant"
    )]
    NotVera,
    #[error("the host grant names no individual Node")]
    Target,
    #[error(transparent)]
    Policy(#[from] ConfigClientError),
    #[error("the provider did not apply the host grant")]
    Provider(#[source] acp::Error),
}

impl ConfigAccess {
    /// The request-bound delegation this access's client mints for granting
    /// `node` the `host` of `agent_doc`; `None` on a home that decides
    /// document access in its own store.
    pub async fn host_credential(
        &self,
        agent_doc: &str,
        node: &str,
    ) -> Result<Option<HostCredential>, ConfigClientError> {
        let Self::Graphql(endpoint) = self else {
            return Ok(None);
        };
        let Some(audience) = self.request_bound_audience().await? else {
            return Ok(None);
        };
        let ids = self.home_policy_ids().await?;
        let node =
            identity::Did::new(node).map_err(ConfigClientError::RelationshipTargetIdentity)?;
        let capability = endpoint.held_client_signing_capability()?;
        let token = StoredCreateCommands::of_host(agent_doc, &node, ids)?
            .mint(capability, &audience)?
            .pop()
            .ok_or(ConfigClientError::ClientConfigurationUpdateInvalid)?;
        Ok(Some(HostCredential {
            token,
            expires_at: audience.issued_at + super::VERA_REQUEST_BOUND_LIFETIME.as_secs(),
        }))
    }

    /// Grant `node` the `host` of `agent_doc` under `credential`.
    pub async fn grant_host_bound(
        &self,
        agent_doc: &str,
        node: &str,
        credential: &HostCredential,
    ) -> Result<(), ConfigClientError> {
        let Self::Graphql(endpoint) = self else {
            return Err(ConfigClientError::ClientAgentTransportRequired);
        };
        let url = format!("{}{HOST_GRANT_PATH}", graphql_api_base(endpoint.url())?);
        let body = HostGrant {
            agent_doc: agent_doc.to_owned(),
            node: node.to_owned(),
            token: credential.token.clone(),
        };
        let client = graphql::http_client().await?;
        let response = endpoint
            .send_resolving(|endpoint| endpoint.authorize(client.post(&url).json(&body)))
            .await??;
        let status = response.status();
        if !status.is_success() {
            return Err(ConfigClientError::HostGrantStatus {
                status,
                body: response.text().await?,
            });
        }
        Ok(())
    }
}

/// Send `account`'s `host` grant of `grant.agent_doc` to `grant.node` to the
/// provider of `node`, bound to `grant.token`. The provider decides whether
/// `account` may grant it and refuses a token bound to another command.
pub(super) async fn submit(
    node: &Arc<defra_node::Node>,
    account: &identity::Did,
    grant: &HostGrant,
) -> Result<(), HostGrantError> {
    if provider_of(node) != DocumentAcp::Vera {
        return Err(HostGrantError::NotVera);
    }
    let target = identity::Did::new(&grant.node).map_err(|_| HostGrantError::Target)?;
    if target.is_wildcard() {
        return Err(HostGrantError::Target);
    }
    let policy = super::client_signed_agent::signing_schema(
        gents_protocol::schemas::AGENT,
        "Agent",
        home_policy_ids(DocumentAcp::Vera),
    )?
    .policy
    .ok_or(ConfigClientError::ClientConfigurationUpdateInvalid)?;
    let context = defra_core::RequestContext::default()
        .with_request_delegation(account.to_string(), grant.token.clone());
    acp::bound_to_request(&node.document_acp(), &context)
        .add_relationship(
            account,
            acp::Subject::entity(target),
            &policy.id,
            &policy.resource_name,
            &grant.agent_doc,
            HOST_RELATION,
            &[],
        )
        .await
        .map_err(HostGrantError::Provider)?;
    Ok(())
}
