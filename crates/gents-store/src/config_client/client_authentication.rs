use gents_protocol::client_authentication::ClientAuthentication;

use super::{ConfigAccess, ConfigClientError, GraphqlEndpoint, Result};

#[derive(Clone)]
pub struct ClientAuthorization {
    actor: identity::Did,
    context: defra_core::RequestContext,
}

impl std::fmt::Debug for ClientAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientAuthorization")
            .field("actor", &self.actor)
            .finish_non_exhaustive()
    }
}

impl ClientAuthorization {
    pub fn actor(&self) -> &identity::Did {
        &self.actor
    }

    pub(crate) fn from_authenticated_bearer(
        actor: identity::Did,
        authorization: &str,
    ) -> Result<Self> {
        let token = authorization
            .strip_prefix("Bearer ")
            .filter(|token| !token.is_empty())
            .ok_or(ConfigClientError::BearerRequired)?;
        let mut token = reqwest::header::HeaderValue::from_str(token)
            .map_err(ConfigClientError::BearerHeader)?;
        token.set_sensitive(true);
        let token = token.to_str().expect("validated bearer header").to_owned();
        let context = defra_core::RequestContext::default()
            .with_identity(Some(actor.to_string()))
            .with_bearer(Some(actor.to_string()), Some(token));
        Ok(Self { actor, context })
    }

    pub fn from_http(
        identity: &defra_http::ExtractIdentity,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<Self> {
        let actor = identity
            .did()
            .cloned()
            .ok_or(ConfigClientError::BearerRequired)?;
        let header = headers
            .get(reqwest::header::AUTHORIZATION)
            .ok_or(ConfigClientError::BearerRequired)?
            .to_str()?;
        let mut authorization = Self::from_authenticated_bearer(actor, header)?;
        let context = identity.context();
        authorization.context = context.with_bearer(
            Some(authorization.actor.to_string()),
            authorization
                .context
                .bearer_token_for(authorization.actor.as_str()),
        );
        Ok(authorization)
    }

    pub(super) fn request_context(
        authorization: Option<&Self>,
        actor: Option<identity::Did>,
    ) -> defra_core::RequestContext {
        let context = match authorization {
            Some(authorization) => authorization.context.clone(),
            None => defra_core::RequestContext::default().with_bearer(None, None),
        };
        context
            .with_identity(actor.map(|actor| actor.to_string()))
            .with_dac_bypass(false)
    }
}

pub fn client_graph_ready(
    node: &defra_node::Node,
    authorization: Option<&ClientAuthorization>,
    signer: &identity::Did,
) -> bool {
    node.local_zanzibar_store().is_some()
        || authorization.is_some_and(|authorization| authorization.actor() == signer)
}

impl ConfigAccess {
    pub fn with_client_authorization(mut self, authorization: ClientAuthorization) -> Result<Self> {
        match &mut self {
            Self::Local(local) => {
                local.attach_client_authorization(authorization)?;
            }
            Self::Graphql(_) => return Err(ConfigClientError::EmbeddedAccessRequired),
        }
        Ok(self)
    }
}

pub async fn runtime_authentication(access: &ConfigAccess) -> Result<ClientAuthentication> {
    access
        .run_local_owned(|node| async move {
            if node.local_zanzibar_store().is_some() {
                return Ok(ClientAuthentication::Local);
            }
            let context = defra_core::RequestContext::default()
                .with_identity(node.node_identity_did().map(str::to_owned));
            let status = node
                .acp_light_client_status_as(&context)
                .await
                .map_err(|error| ConfigClientError::RuntimeAuthenticationRead(Box::new(error)))?;
            let audience = status
                .delegation
                .ok_or(ConfigClientError::RuntimeAuthenticationUnavailable)?;
            Ok(ClientAuthentication::Vera {
                publisher_node: crate::local::document_acp::transport_publisher_node(&node)
                    .map_err(|_| ConfigClientError::RuntimeAuthenticationUnavailable)?,
                worker: audience.worker,
                deployment: audience.deployment,
            })
        })
        .await
}

impl GraphqlEndpoint {
    pub async fn client_authorization(&self) -> Result<Option<ClientAuthorization>> {
        let Some(actor) = self.principal_did.as_deref() else {
            return Ok(None);
        };
        let Some(authorization) = self.runtime_authorization().await? else {
            return Ok(None);
        };
        let actor =
            identity::Did::new(actor).map_err(ConfigClientError::RelationshipTargetIdentity)?;
        ClientAuthorization::from_authenticated_bearer(actor, &authorization).map(Some)
    }

    pub async fn requires_client_delegation(&self) -> Result<bool> {
        self.runtime_authentication().await?;
        Ok(self.mints_delegations())
    }

    pub async fn runtime_authorization(&self) -> Result<Option<String>> {
        self.runtime_authentication().await?;
        self.authorization()
    }

    pub async fn authorize_runtime(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        self.runtime_authentication().await?;
        self.authorize(request)
    }
}

#[cfg(test)]
mod conformance;
