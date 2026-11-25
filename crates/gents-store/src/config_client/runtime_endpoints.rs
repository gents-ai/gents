use std::sync::{Arc, RwLock};

use gents_protocol::client_authentication::ClientAuthentication;
use gents_protocol::delegation_audience::HeldAudienceChange;
use gents_protocol::home_attestation::{HomeNonce, HomeStatusRefusal, HOME_NONCE_QUERY};
use rapidhash::RapidHashMap;

use super::home_status::{self, AuthenticatedHome};
use super::{ClientSigningCapability, ConfigClientError, GraphqlEndpoint, Result};

#[derive(Debug)]
pub(super) enum EndpointAuthentication {
    Fixed(ClientAuthentication),
    Runtime {
        node: String,
        current: tokio::sync::Mutex<Option<ClientAuthentication>>,
    },
}

impl PartialEq for EndpointAuthentication {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fixed(left), Self::Fixed(right)) => left == right,
            (Self::Runtime { node: left, .. }, Self::Runtime { node: right, .. }) => left == right,
            _ => false,
        }
    }
}

impl Eq for EndpointAuthentication {}

pub struct RuntimeEndpoints {
    capability: ClientSigningCapability,
    endpoints: RwLock<RapidHashMap<(String, String), GraphqlEndpoint>>,
    bearer_lifetime: Option<std::time::Duration>,
}

impl RuntimeEndpoints {
    pub fn new(capability: ClientSigningCapability) -> Self {
        Self {
            capability,
            endpoints: RwLock::default(),
            bearer_lifetime: None,
        }
    }

    /// Every endpoint opened from now on mints bearers of `lifetime`
    /// ([`GraphqlEndpoint::with_bearer_lifetime`]).
    pub fn with_bearer_lifetime(mut self, lifetime: std::time::Duration) -> Self {
        self.bearer_lifetime = Some(lifetime);
        self
    }

    pub fn endpoint(&self, node: &str, url: &str) -> Result<GraphqlEndpoint> {
        let key = (node.to_owned(), url.to_owned());
        let mut endpoints = self
            .endpoints
            .write()
            .map_err(|_| ConfigClientError::RuntimeEndpointCache)?;
        if let Some(endpoint) = endpoints.get(&key) {
            return Ok(endpoint.clone());
        }
        let mut endpoint = GraphqlEndpoint::as_principal(url, self.capability.did())
            .with_client_signing_capability(self.capability.clone())?;
        if let Some(lifetime) = self.bearer_lifetime {
            endpoint = endpoint.with_bearer_lifetime(lifetime);
        }
        endpoint.client_authentication = Arc::new(EndpointAuthentication::Runtime {
            node: node.to_owned(),
            current: tokio::sync::Mutex::new(None),
        });
        endpoints.insert(key, endpoint.clone());
        Ok(endpoint)
    }
}

impl GraphqlEndpoint {
    /// The runtime this endpoint was opened for, when it was opened for one.
    pub fn runtime_node_did(&self) -> Option<&str> {
        match &*self.client_authentication {
            EndpointAuthentication::Runtime { node, .. } => Some(node),
            _ => None,
        }
    }

    /// Discover the runtime's client authentication again, holding the
    /// held audience's status lock as every status read does (TLA+
    /// `HeldAudienceReads`).
    pub async fn refresh_runtime_authentication(&self) -> Result<()> {
        if let EndpointAuthentication::Runtime { node, current } = &*self.client_authentication {
            let _reading = self.delegation_audience.reading().await;
            *current.lock().await = None;
            self.discover_runtime_authentication(node).await?;
        }
        Ok(())
    }

    pub(super) async fn runtime_authentication(&self) -> Result<ClientAuthentication> {
        match &*self.client_authentication {
            EndpointAuthentication::Fixed(authentication) => Ok(authentication.clone()),
            EndpointAuthentication::Runtime { node, current } => {
                let known = current.lock().await.clone();
                if let Some(authentication) = known {
                    return Ok(authentication);
                }
                let _reading = self.delegation_audience.reading().await;
                let known = current.lock().await.clone();
                match known {
                    Some(authentication) => Ok(authentication),
                    None => self.discover_runtime_authentication(node).await,
                }
            }
        }
    }

    /// Read the runtime's status and keep it; the caller holds the status
    /// lock.
    async fn discover_runtime_authentication(&self, node: &str) -> Result<ClientAuthentication> {
        let home = self
            .fetch_runtime_status(node)
            .await?
            .map_err(ConfigClientError::HomeStatusUnauthenticated)?;
        self.keep_runtime_status(&home).await;
        Ok(home.authentication().clone())
    }

    /// Keep what a runtime's own status reports: its client authentication,
    /// then its audience and clock; the caller holds the status lock.
    pub(super) async fn keep_runtime_status(&self, home: &AuthenticatedHome) -> HeldAudienceChange {
        if let EndpointAuthentication::Runtime { current, .. } = &*self.client_authentication {
            *current.lock().await = Some(home.authentication().clone());
        }
        self.delegation_audience.observe(home)
    }

    /// Read a runtime endpoint's home's status, keeping nothing.
    pub(super) async fn fetch_runtime_home(&self) -> Option<Home> {
        let EndpointAuthentication::Runtime { node, .. } = &*self.client_authentication else {
            return None;
        };
        self.fetch_runtime_status(node).await.ok()
    }

    async fn fetch_runtime_status(&self, node: &str) -> Result<Home> {
        let base = self
            .url()
            .trim()
            .strip_suffix("/api/v0/graphql")
            .ok_or_else(|| ConfigClientError::GraphqlApiBase {
                graphql: self.to_string(),
            })?;
        let nonce = home_status::fresh_nonce();
        let response = discovery_client()
            .await?
            .get(format!("{base}/status"))
            .query(&[(HOME_NONCE_QUERY, nonce.as_str())])
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(ConfigClientError::RuntimeAuthenticationHttp)?;
        if !response.status().is_success() {
            return Err(ConfigClientError::RuntimeAuthenticationStatus(
                response.status(),
            ));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(ConfigClientError::RuntimeAuthenticationHttp)?;
        Self::decode_runtime_status(node, &nonce, &body)
    }

    fn decode_runtime_status(
        node: &str,
        nonce: &HomeNonce,
        body: &serde_json::Value,
    ) -> Result<Home> {
        if body.get("node_did").and_then(serde_json::Value::as_str) != Some(node) {
            return Err(ConfigClientError::RuntimeAuthenticationIdentity);
        }
        match home_status::authenticate(body, node, nonce).and_then(|home| home.resolved()) {
            Ok(Some(home)) => Ok(Ok(home)),
            Ok(None) => Err(ConfigClientError::RuntimeAuthenticationUnavailable),
            Err(refusal) => Ok(Err(refusal)),
        }
    }
}

/// What a status read reports for the saved home, or why it is not the
/// home's.
pub(super) type Home = std::result::Result<AuthenticatedHome, HomeStatusRefusal>;

pub(super) async fn discovery_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(30))
                    .build()
                    .map_err(ConfigClientError::HttpClientBuild)
            })
            .await
            .map_err(ConfigClientError::HttpClientJoin)?
        })
        .await
}

#[cfg(test)]
mod tests;
