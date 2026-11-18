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
        let Some(authorization) = self.authorization().await? else {
            return Ok(None);
        };
        let actor =
            identity::Did::new(actor).map_err(ConfigClientError::RelationshipTargetIdentity)?;
        ClientAuthorization::from_authenticated_bearer(actor, &authorization).map(Some)
    }

    pub async fn requires_client_delegation(&self) -> Result<bool> {
        Ok(matches!(
            self.runtime_authentication().await?,
            ClientAuthentication::Vera { .. }
        ))
    }

    pub async fn authorization(&self) -> Result<Option<String>> {
        if let Some(value) = &self.delegated_authorization {
            return Ok(Some(value.to_str()?.to_owned()));
        }
        let Some(did) = self.principal_did.as_deref() else {
            return Ok(None);
        };
        let capability = self
            .client_signing_capability
            .as_ref()
            .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
        let authentication = self.runtime_authentication().await?;
        let authorization = match &authentication {
            gents_protocol::client_authentication::ClientAuthentication::Local => {
                crate::identity::defradb_bearer_authorization_with_config(
                    did,
                    &self.audience()?,
                    &capability.config,
                )?
            }
            gents_protocol::client_authentication::ClientAuthentication::Vera {
                worker,
                deployment,
            } => {
                let signer =
                    vera::vera_rs::Ed25519Signer::from_slice(&capability.config.private_key_bytes)
                        .map_err(|error| {
                            ConfigClientError::ClientDelegationSigner(Box::new(error))
                        })?;
                let audience = acp::DelegationAudience {
                    genesis_id: None,
                    worker: worker.clone(),
                    deployment: *deployment,
                };
                let token = tokio::spawn(async move {
                    vera::vera_rs::caller_delegation(
                        &signer,
                        &audience,
                        crate::identity::DEFRADB_BEARER_LIFETIME,
                    )
                    .await
                })
                .await
                .map_err(ConfigClientError::ClientDelegationTask)?
                .map_err(|error| ConfigClientError::ClientDelegation(Box::new(error)))?;
                format!("Bearer {token}")
            }
            gents_protocol::client_authentication::ClientAuthentication::Unavailable => {
                return Err(ConfigClientError::RuntimeAuthenticationUnavailable);
            }
        };
        Ok(Some(authorization))
    }

    async fn authorization_header(&self) -> Result<Option<reqwest::header::HeaderValue>> {
        let Some(authorization) = self.authorization().await? else {
            return Ok(None);
        };
        let mut value = reqwest::header::HeaderValue::from_str(&authorization)
            .map_err(ConfigClientError::BearerHeader)?;
        value.set_sensitive(true);
        Ok(Some(value))
    }

    pub async fn authorize(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        Ok(match self.authorization_header().await? {
            Some(value) => request.header(reqwest::header::AUTHORIZATION, value),
            None => request,
        })
    }
}

#[cfg(test)]
mod conformance;
