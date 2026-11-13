use gents_documents::inference_backend::observation_binding;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub node_did: String,
    pub display_name: Option<String>,
    pub default_agent_did: Option<String>,
    pub enabled: Option<bool>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub creation_nonce: String,
    pub agent_did: String,
    pub node_did: String,
    pub display_name: String,
    pub description: Option<String>,
    pub context_id: Option<String>,
    pub inference_profile_id: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub tags: Vec<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InferenceBackendView {
    pub backend_id: String,
    pub name: Option<String>,
    pub provider_kind: Option<String>,
    pub openai_wire_api: Option<String>,
    pub endpoint: Option<String>,
    pub auth_kind: Option<String>,
    pub connect_timeout_secs: Option<i64>,
    pub discovery_timeout_secs: Option<i64>,
    pub api_key_configured: bool,
    pub api_key_env_var: Option<String>,
    pub max_concurrent: Option<i64>,
    pub max_queue_depth: Option<i64>,
    pub enabled: Option<bool>,
    pub tags: Vec<String>,
    pub models: Vec<String>,
    pub advertised_models: Vec<gents_documents::AdvertisedModel>,
    pub probe_status: Option<String>,
    /// The account a subscription backend runs on; `None` is the original.
    pub account_ref: Option<String>,
    /// Whether a pool backend may draw paid extra usage; `None` for any other backend.
    pub allow_overage: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InferenceSetupState {
    pub node_did: String,
    pub node: NodeView,
    pub agents: Vec<AgentView>,
    pub agent_configs: Vec<gents_documents::Agent>,
    pub inference_backends: Vec<InferenceBackendView>,
    pub inference_profiles: Vec<gents_documents::InferenceProfile>,
    pub fast_mode_backend_ids: Vec<String>,
}

pub fn backend_config_view(
    row: &gents_documents::InferenceBackend,
    observation: Option<&gents_documents::InferenceBackendObservation>,
) -> InferenceBackendView {
    use gents_documents::BackendAuth;
    let (auth_kind, api_key_configured, api_key_env_var) = match &row.auth {
        BackendAuth::Unauthenticated => ("unauthenticated", false, None),
        BackendAuth::ApiKey { .. } => ("api_key", true, None),
        BackendAuth::Credential { .. } => ("credential", true, None),
        BackendAuth::Environment { variable } => ("environment", false, Some(variable.clone())),
        BackendAuth::NodeOAuth { .. } => ("node_oauth", false, None),
        BackendAuth::NodeOAuthPool { .. } => ("node_oauth_pool", false, None),
        BackendAuth::AwsCredentials { .. } => ("aws_credentials", false, None),
        BackendAuth::GoogleCredentials { .. } => ("google_credentials", false, None),
    };
    let advertised_models = match gents_inference::resolved::backend_catalog(row, observation) {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(backend_id = %row.backend_id, node_did = %row.node_did,
            %error, "cannot project ambiguous backend catalog");
            None
        }
    }
    .map(|catalog| catalog.models.clone())
    .unwrap_or_default();
    let models = advertised_models
        .iter()
        .map(|model| model.model_name.clone())
        .collect();
    InferenceBackendView {
        backend_id: row.backend_id.clone(),
        name: Some(row.name.clone()),
        provider_kind: Some(row.provider_kind.as_str().to_owned()),
        openai_wire_api: row.openai_wire_api.map(|api| api.as_str().to_owned()),
        endpoint: Some(row.endpoint.clone()),
        auth_kind: Some(auth_kind.to_owned()),
        connect_timeout_secs: row.connect_timeout_secs,
        discovery_timeout_secs: row.discovery_timeout_secs,
        api_key_configured,
        api_key_env_var,
        max_concurrent: row.max_concurrent,
        max_queue_depth: row.max_queue_depth,
        enabled: Some(row.enabled),
        tags: row.tags.clone(),
        models,
        advertised_models,
        probe_status: observation.and_then(|observation| observation.probe_status.clone()),
        account_ref: row.auth.oauth_account_ref().map(str::to_owned),
        allow_overage: row.auth.pool_allow_overage(),
    }
}

impl InferenceSetupState {
    pub fn project(store: &crate::client::ClientStore, node_did: &str) -> Self {
        let node_row = store.nodes.iter().find(|row| row.node_did == node_did);
        let node = node_row
            .map(|row| NodeView {
                node_did: row.node_did.clone(),
                display_name: normalize_optional(row.display_name.as_deref()),
                default_agent_did: store
                    .selected_default_agent_did(node_did)
                    .map(str::to_owned),
                enabled: Some(row.enabled),
                created_at: normalize_optional(row.created_at.as_deref()),
                created_by: normalize_optional(row.created_by.as_deref()),
            })
            .unwrap_or_else(|| NodeView {
                node_did: node_did.to_owned(),
                display_name: None,
                default_agent_did: None,
                enabled: None,
                created_at: None,
                created_by: None,
            });
        let mut agent_configs = store
            .agents
            .iter()
            .filter(|row| row.node_did == node_did)
            .cloned()
            .collect::<Vec<_>>();
        agent_configs.sort_by(|left, right| left.agent_did.cmp(&right.agent_did));
        let default_agent_did = store
            .default_agent_did_for_node(node_did)
            .map(str::to_owned);
        let mut agents = store
            .agent_rows(node_did)
            .into_iter()
            .map(|row| AgentView {
                creation_nonce: row.creation_nonce.clone(),
                agent_did: row.agent_did.clone(),
                node_did: row.node_did.clone(),
                display_name: normalize_optional(row.display_name.as_deref())
                    .unwrap_or_else(|| row.agent_did.clone()),
                description: row.description.clone(),
                context_id: row.context_id.clone(),
                inference_profile_id: Some(row.inference_profile_id.clone()),
                enabled: row.enabled,
                is_default: default_agent_did.as_deref() == Some(row.agent_did.as_str()),
                tags: row.tags.clone(),
                created_at: row.created_at.clone(),
            })
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| {
            right
                .is_default
                .cmp(&left.is_default)
                .then_with(|| left.display_name.cmp(&right.display_name))
        });
        let mut inference_backends = store
            .inference_backends
            .iter()
            .filter(|row| row.node_did == node_did)
            .map(|row| {
                let observation = row.physical_doc_id.as_deref().and_then(|metadata_doc| {
                    observation_binding::bind(
                        &row.node_did,
                        &row.backend_id,
                        metadata_doc,
                        store.backend_observations.iter().map(|observation| {
                            (
                                observation_binding::ObservationKey {
                                    node_did: Some(node_did),
                                    backend_id: &observation.backend_id,
                                    metadata_doc_id: observation.metadata_doc_id.as_deref(),
                                },
                                observation,
                            )
                        }),
                    )
                    .ok()
                    .flatten()
                });
                backend_config_view(row, observation)
            })
            .collect::<Vec<_>>();
        inference_backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));

        let mut inference_profiles = store
            .inference_profiles
            .iter()
            .filter(|row| row.node_did == node_did)
            .cloned()
            .collect::<Vec<_>>();
        inference_profiles.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
        Self {
            node_did: node_did.to_owned(),
            node,
            agents,
            agent_configs,
            fast_mode_backend_ids: fast_mode_backend_ids(&inference_backends),
            inference_backends,
            inference_profiles,
        }
    }
}

pub fn fast_mode_backend_ids(backends: &[InferenceBackendView]) -> Vec<String> {
    backends
        .iter()
        .filter_map(|backend| {
            let kind = gents_inference::backends::provider::BackendProviderKind::parse_optional(
                backend.provider_kind.as_deref(),
            )
            .ok()?;
            gents_inference::resolved::supports_service_tier(kind)
                .then(|| backend.backend_id.clone())
        })
        .collect()
}

/// A failed model discovery's kind, whether the provider answered, and whether the user may
/// type a model id it did not advertise.
fn classify_discovery_failure(error: &anyhow::Error) -> (&'static str, bool, bool) {
    let http_error = error.chain().find_map(|cause| {
        cause.downcast_ref::<gents_inference::backends::provider::ModelDiscoveryHttpError>()
    });
    let is_decode = error
        .chain()
        .any(|cause| cause.downcast_ref::<serde_json::Error>().is_some());
    if let Some(error) = http_error {
        if error.is_auth() {
            ("authentication", true, false)
        } else {
            ("http", true, true)
        }
    } else if is_decode {
        ("decode", true, true)
    } else {
        ("connection", false, false)
    }
}

const DISCOVERY_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Why a model discovery has no answer to show.
#[derive(Debug, thiserror::Error)]
pub enum ModelDiscoveryError {
    #[error(transparent)]
    Connection(anyhow::Error),
    #[error("API key is required")]
    KeyRequired,
    /// A sign-in needs the node's configuration, which the caller could not reach.
    #[error("node configuration is not reachable")]
    NoAccess,
    /// Reading the node's provider accounts failed.
    #[error(transparent)]
    Accounts(anyhow::Error),
    #[error("the selected provider account is not connected")]
    NotConnected,
    #[error(transparent)]
    Client(reqwest::Error),
    #[error(transparent)]
    Model(anyhow::Error),
}

/// What the provider answered: the connection asked, its models, or why there are none.
pub struct ModelDiscovery {
    pub spec: gents_inference::backends::setup::InferenceConnectionSpec,
    pub reachable: bool,
    pub models: Vec<gents_inference::backends::setup::InferenceModelOption>,
    /// The failure's kind and text, which may carry what the provider echoed.
    pub failure: Option<(&'static str, String)>,
    pub manual_entry_allowed: bool,
}

/// The connection test of inference setup: lists the provider's models with the key or the
/// node's stored sign-in, and records them on that connection's backends. Without `access` a
/// sign-in cannot be read and nothing is recorded.
pub async fn discover_models(
    access: Option<&gents_store::config_client::ConfigAccess>,
    node_did: &str,
    provider: gents_inference::backends::setup::InferenceProviderId,
    method: gents_inference::backends::setup::InferenceAuthMethod,
    endpoint: &str,
    api_key: Option<&str>,
    account_ref: Option<&str>,
) -> Result<ModelDiscovery, ModelDiscoveryError> {
    use gents_inference::backends::setup::{connection_spec, model_option};
    let spec =
        connection_spec(provider, method, endpoint).map_err(ModelDiscoveryError::Connection)?;
    let api_key = api_key.map(str::trim).filter(|key| !key.is_empty());
    if spec.api_key_required && api_key.is_none() {
        return Err(ModelDiscoveryError::KeyRequired);
    }
    let (credential, oauth_auth) = match spec.oauth_provider {
        Some(oauth) => {
            let access = access.ok_or(ModelDiscoveryError::NoAccess)?;
            let (credential, auth) = discovery_account(access, node_did, oauth, account_ref)
                .await
                .map_err(ModelDiscoveryError::Accounts)?;
            let credential = credential.ok_or(ModelDiscoveryError::NotConnected)?;
            (Some(credential), Some(auth))
        }
        None => (None, None),
    };
    let client = reqwest::Client::builder()
        .connect_timeout(DISCOVERY_WAIT)
        .timeout(DISCOVERY_WAIT)
        .build()
        .map_err(ModelDiscoveryError::Client)?;
    let found = gents_inference::backends::provider::discover_models(
        &client,
        spec.provider_kind,
        &spec.endpoint,
        api_key,
        credential.as_ref(),
    )
    .await;
    let models = match found {
        Ok(models) => models,
        Err(error) => {
            let (kind, reachable, manual_entry_allowed) = classify_discovery_failure(&error);
            return Ok(ModelDiscovery {
                spec,
                reachable,
                models: Vec::new(),
                failure: Some((kind, format!("{error:#}"))),
                manual_entry_allowed,
            });
        }
    };
    if let Some(access) = access {
        use gents_documents::BackendAuth;
        let auth = match (oauth_auth, api_key) {
            (Some(auth), _) => auth,
            (None, Some(key)) => BackendAuth::ApiKey {
                key: key.to_string(),
            },
            (None, None) => BackendAuth::Unauthenticated,
        };
        let recorded = gents_inference::backends::registry::record_connection_catalog_on(
            access,
            node_did,
            spec.provider_kind,
            &spec.endpoint,
            &auth,
            models.clone(),
        )
        .await;
        if let Err(error) = recorded {
            let mut error = format!("{error:#}");
            // A store refusal may echo the backend's key.
            if let Some(key) = api_key {
                error = error.replace(key, "…");
            }
            tracing::warn!(
                node_did,
                %error,
                "recording the discovered model catalog failed; the previous catalog is kept"
            );
        }
    }
    if models.is_empty() {
        return Ok(ModelDiscovery {
            spec,
            reachable: true,
            models: Vec::new(),
            failure: Some((
                "empty_catalog",
                "The connection succeeded, but the provider advertised no models.".into(),
            )),
            manual_entry_allowed: true,
        });
    }
    let models = models
        .into_iter()
        .map(|model| model_option(provider, method, model))
        .collect::<anyhow::Result<_>>()
        .map_err(ModelDiscoveryError::Model)?;
    Ok(ModelDiscovery {
        spec,
        reachable: true,
        models,
        failure: None,
        manual_entry_allowed: false,
    })
}

/// The stored sign-in a discovery on `account_ref` uses, and the auth its backends record.
pub async fn discovery_account(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    provider: &str,
    account_ref: Option<&str>,
) -> anyhow::Result<(
    Option<gents_inference::auth::credential::OAuthCredential>,
    gents_documents::BackendAuth,
)> {
    let credential = gents_inference::auth::credential::resolve_oauth_credential(
        access,
        node_did,
        provider,
        gents_inference::auth::credential::AccountPick::Reference(account_ref),
    )
    .await?;
    Ok((
        credential,
        gents_documents::BackendAuth::NodeOAuth {
            account_ref: account_ref.map(str::to_owned),
        },
    ))
}

pub async fn load_inference_setup_state(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
) -> anyhow::Result<InferenceSetupState> {
    let store = crate::client::load_inference_configuration(access, node_did).await?;
    anyhow::ensure!(
        store
            .operator_config_authoritative_node_dids
            .contains(node_did),
        "runtime inference configuration is incomplete"
    );
    anyhow::ensure!(
        store
            .nodes
            .iter()
            .filter(|row| row.node_did == node_did)
            .count()
            == 1,
        "runtime inference configuration requires exactly one node"
    );
    Ok(InferenceSetupState::project(&store, node_did))
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

pub const INFERENCE_SETTINGS_CONTRACT_VERSION: u32 = 1;
pub const INFERENCE_SETTINGS_PATH: &str = "/inference/settings";
/// Bound an endpoint's response before retaining and decoding its JSON body.
const INFERENCE_SETTINGS_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InferenceSettingsResponse {
    pub contract_version: u32,
    pub state: InferenceSetupState,
}

impl InferenceSetupState {
    /// Client metadata excludes credentials and credential-bearing endpoint URLs.
    pub fn for_client_observation(mut self) -> Self {
        for backend in &mut self.inference_backends {
            backend.endpoint = backend.endpoint.take().filter(|endpoint| {
                reqwest::Url::parse(endpoint).is_ok_and(|url| {
                    url.username().is_empty()
                        && url.password().is_none()
                        && url.query().is_none()
                        && url.fragment().is_none()
                })
            });
            backend.api_key_env_var = None;
        }
        self
    }
}

impl crate::client::ClientCore {
    pub async fn read_inference_setup_state(
        &self,
        node_did: &str,
    ) -> anyhow::Result<InferenceSetupState> {
        use anyhow::Context;
        let records = self.peer_records().await;
        let mut matching = records.iter().filter(|record| record.node_did == node_did);
        let record = matching
            .next()
            .filter(|_| matching.next().is_none())
            .context("runtime connection is missing or ambiguous")?;
        let graphql = record
            .graphql
            .as_deref()
            .context("runtime has no HTTP endpoint")?;
        read_inference_setup_state_from(graphql, node_did, self.node_identity()).await
    }
}

/// A runtime endpoint can be unavailable or unresponsive while settings are
/// opened; bound the HTTP wait as well as the response body.
async fn read_inference_setup_state_from(
    graphql: &str,
    node_did: &str,
    client_identity: &crate::client::PrincipalIdentity,
) -> anyhow::Result<InferenceSetupState> {
    use anyhow::Context;
    use gents_store::config_client::GraphqlEndpoint;
    let mut url = reqwest::Url::parse(graphql).context("runtime endpoint is invalid")?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "runtime endpoint must be an HTTP URL without credentials"
    );
    let base = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .context("runtime endpoint has no GraphQL path")?;
    let path = format!("{base}{INFERENCE_SETTINGS_PATH}");
    url.set_path(&path);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let capability = gents_store::config_client::ClientSigningCapability::from_ed25519_key(
        client_identity.private_key_bytes(),
    )?;
    let endpoint = GraphqlEndpoint::as_principal(graphql, client_identity.did())
        .with_client_signing_capability(capability)?;
    let mut response = endpoint
        .authorize(client.get(url))?
        .send()
        .await
        .map_err(|error| {
            crate::error::BridgeError::runtime_http_error(
                error,
                "reading runtime model settings".to_owned(),
            )
        })?;
    anyhow::ensure!(
        response.status().is_success(),
        "runtime model settings are unavailable (HTTP {}); reconnect the runtime and try again",
        response.status().as_u16()
    );
    anyhow::ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= INFERENCE_SETTINGS_MAX_BYTES as u64),
        "runtime model settings exceed the response limit"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        let message = error.to_string();
        crate::error::BridgeError::runtime_http_error(error, message)
    })? {
        anyhow::ensure!(
            bytes.len().saturating_add(chunk.len()) <= INFERENCE_SETTINGS_MAX_BYTES,
            "runtime model settings exceed the response limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    let response: InferenceSettingsResponse =
        serde_json::from_slice(&bytes).context("decoding runtime model settings")?;
    anyhow::ensure!(
        response.contract_version == INFERENCE_SETTINGS_CONTRACT_VERSION,
        "runtime model settings require a matching runtime version"
    );
    anyhow::ensure!(
        response.state.node_did == node_did && response.state.node.node_did == node_did,
        "runtime model settings belong to a different node"
    );
    Ok(response.state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{ClientStore, DesktopPaths, PrincipalIdentity};
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn settings(node_did: &str) -> InferenceSetupState {
        InferenceSetupState::project(&ClientStore::default(), node_did)
    }

    async fn client_identity() -> (tempfile::TempDir, PrincipalIdentity) {
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path());
        paths.ensure_root_dirs().await.unwrap();
        let identity = PrincipalIdentity::load_or_create(&paths).await.unwrap();
        (temp, identity)
    }

    #[tokio::test]
    async fn inference_settings_read_uses_the_loaded_client_identity() {
        let (_temp, identity) = client_identity().await;
        let server = MockServer::start().await;
        let node = "did:runtime";
        Mock::given(method("GET"))
            .and(path(INFERENCE_SETTINGS_PATH))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(InferenceSettingsResponse {
                    contract_version: INFERENCE_SETTINGS_CONTRACT_VERSION,
                    state: settings(node),
                }),
            )
            .expect(1)
            .mount(&server)
            .await;
        let state = read_inference_setup_state_from(
            &format!("{}/api/v0/graphql", server.uri()),
            node,
            &identity,
        )
        .await
        .unwrap();
        assert_eq!(state.node_did, node);
        let requests = server.received_requests().await.unwrap();
        let authorization = requests[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(authorization.starts_with("Bearer "));
        assert!(
            gents_store::identity::defradb_bearer_authorization(node, "localhost").is_err(),
            "the runtime signer must remain unavailable"
        );
    }

    #[tokio::test]
    async fn inference_settings_read_rejects_foreign_nodes_and_contracts() {
        let (_temp, identity) = client_identity().await;
        for (version, outer_node, inner_node) in [
            (2, "did:runtime", "did:runtime"),
            (1, "did:other", "did:runtime"),
            (1, "did:runtime", "did:other"),
        ] {
            let server = MockServer::start().await;
            let mut state = settings(outer_node);
            state.node.node_did = inner_node.into();
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(InferenceSettingsResponse {
                        contract_version: version,
                        state,
                    }),
                )
                .mount(&server)
                .await;
            assert!(read_inference_setup_state_from(
                &format!("{}/api/v0/graphql", server.uri()),
                "did:runtime",
                &identity
            )
            .await
            .is_err());
        }
    }

    #[tokio::test]
    async fn inference_settings_read_does_not_follow_redirects_or_expose_errors() {
        let (_temp, identity) = client_identity().await;
        let target = MockServer::start().await;
        for status in [302, 403, 503] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("Location", target.uri())
                        .set_body_string("private-runtime-error"),
                )
                .mount(&server)
                .await;
            let error = read_inference_setup_state_from(
                &format!("{}/api/v0/graphql", server.uri()),
                "did:runtime",
                &identity,
            )
            .await
            .unwrap_err();
            assert!(!format!("{error:#}").contains("private-runtime-error"));
        }
        assert!(target.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn inference_settings_read_bounds_response_size() {
        let (_temp, identity) = client_identity().await;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b' ';
                INFERENCE_SETTINGS_MAX_BYTES
                    + 1
            ]))
            .mount(&server)
            .await;
        let error = read_inference_setup_state_from(
            &format!("{}/api/v0/graphql", server.uri()),
            "did:runtime",
            &identity,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("response limit"));
    }

    #[tokio::test]
    async fn inference_metadata_requires_complete_scoped_configuration() {
        use gents_store::test_support::wire_database::WireDatabase;

        let node = "did:runtime";
        let row = || json!({"_docID": "node", "node_did": node});
        for (name, rows, accepted) in [
            ("complete", json!({"Node": [row()]}), true),
            (
                "omitted backends",
                json!({"Node": [row()], "InferenceBackendMetadata": null}),
                true,
            ),
            ("missing node", json!({}), false),
            ("omitted node", json!({"Node": null}), false),
            ("duplicate node", json!({"Node": [row(), row()]}), false),
            (
                "malformed agent",
                json!({"Node": [row()], "Agent": [{"agent_did": "incomplete"}]}),
                false,
            ),
        ] {
            let read = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let database = WireDatabase::spawn(rows, {
                let read = read.clone();
                std::sync::Arc::new(move |collection| {
                    read.lock().unwrap().push(collection);
                    Box::pin(async { Ok(()) })
                })
            })
            .await;
            let access = gents_store::config_client::ConfigAccess::Graphql(database.reader());
            assert_eq!(
                load_inference_setup_state(&access, node).await.is_ok(),
                accepted,
                "{name}"
            );
            let read = read.lock().unwrap();
            assert!(read.iter().any(|collection| collection == "Node"));
            assert!(read
                .iter()
                .any(|collection| collection == "InferenceBackendMetadata"));
            assert!(!read
                .iter()
                .any(|collection| collection == "AgentRequest" || collection == "AgentSession"));
        }
    }

    #[test]
    fn inference_metadata_never_exports_backend_credentials() {
        let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
            "node_did": "did:runtime", "backend_id": "backend", "name": "Provider",
            "provider_kind": "OpenAiCompatible", "openai_wire_api": "chat_completions",
            "endpoint": "https://user:private-url-password@example.com/v1?key=private-query-key",
            "auth": {"kind":"api_key", "key":"private-api-key"}
        }))
        .unwrap();
        let mut state = settings("did:runtime");
        state
            .inference_backends
            .push(backend_config_view(&backend, None));
        let state = state.for_client_observation();
        assert!(state.inference_backends[0].api_key_configured);
        assert_eq!(state.inference_backends[0].endpoint, None);
        let encoded = serde_json::to_string(&state).unwrap();
        for secret in [
            "private-url-password",
            "private-query-key",
            "private-api-key",
        ] {
            assert!(!encoded.contains(secret));
        }
    }
}
