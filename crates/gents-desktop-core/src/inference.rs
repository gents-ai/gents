use crate::client::source_matches_node;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub node_did: String,
    pub display_name: Option<String>,
    pub default_agent_id: Option<String>,
    pub enabled: Option<bool>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub agent_id: String,
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
    pub advertised_models: Vec<gents::document_config::AdvertisedModel>,
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
    pub agent_configs: Vec<gents::document_config::Agent>,
    pub inference_backends: Vec<InferenceBackendView>,
    pub inference_profiles: Vec<gents::document_config::InferenceProfile>,
    pub fast_mode_backend_ids: Vec<String>,
}

pub fn backend_config_view(
    row: &gents::document_config::InferenceBackend,
    observation: Option<&gents::document_config::InferenceBackendObservation>,
) -> InferenceBackendView {
    use gents::document_config::BackendAuth;
    let (auth_kind, api_key_configured, api_key_env_var) = match &row.auth {
        BackendAuth::Unauthenticated => ("unauthenticated", false, None),
        BackendAuth::ApiKey { .. } => ("api_key", true, None),
        BackendAuth::Environment { variable } => ("environment", false, Some(variable.clone())),
        BackendAuth::NodeOAuth { .. } => ("node_oauth", false, None),
        BackendAuth::NodeOAuthPool { .. } => ("node_oauth_pool", false, None),
    };
    let advertised_models = match gents::config::backend_catalog(row, observation) {
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
                default_agent_id: normalize_optional(row.default_agent_id.as_deref()),
                enabled: Some(row.enabled),
                created_at: normalize_optional(row.created_at.as_deref()),
                created_by: normalize_optional(row.created_by.as_deref()),
            })
            .unwrap_or_else(|| NodeView {
                node_did: node_did.to_owned(),
                display_name: None,
                default_agent_id: None,
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
        agent_configs.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        let default_agent_id = store.default_agent_id_for_node(node_did).map(str::to_owned);
        let mut agents = store
            .agent_rows(node_did)
            .into_iter()
            .map(|row| AgentView {
                agent_id: row.agent_id.clone(),
                node_did: row.node_did.clone(),
                display_name: normalize_optional(row.display_name.as_deref())
                    .unwrap_or_else(|| row.agent_id.clone()),
                description: row.description.clone(),
                context_id: row.context_id.clone(),
                inference_profile_id: Some(row.inference_profile_id.clone()),
                enabled: row.enabled,
                is_default: default_agent_id.as_deref() == Some(row.agent_id.as_str()),
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
                let observation = store
                    .backend_observations
                    .iter()
                    .enumerate()
                    .find(|(index, observation)| {
                        observation.backend_id == row.backend_id
                            && source_matches_node(
                                &store.backend_observation_source_node_dids,
                                *index,
                                &row.node_did,
                                true,
                            )
                    })
                    .map(|(_, observation)| observation);
                backend_config_view(row, observation)
            })
            .collect::<Vec<_>>();
        inference_backends.sort_by(|left, right| left.backend_id.cmp(&right.backend_id));

        let mut inference_profiles = store
            .inference_profiles
            .iter()
            .enumerate()
            .filter(|(index, row)| {
                row.node_did == node_did
                    && source_matches_node(
                        &store.inference_profile_source_node_dids,
                        *index,
                        node_did,
                        false,
                    )
            })
            .map(|(_, row)| row.clone())
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
            let kind = gents::BackendProviderKind::parse_optional(backend.provider_kind.as_deref())
                .ok()?;
            gents::config::supports_service_tier(kind).then(|| backend.backend_id.clone())
        })
        .collect()
}

pub async fn load_inference_setup_state(
    access: &gents::config_client::ConfigAccess,
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
        read_inference_setup_state_from(graphql, node_did, self.node_identity().did()).await
    }
}

/// A runtime endpoint can be unavailable or unresponsive while settings are
/// opened; bound the HTTP wait as well as the response body.
async fn read_inference_setup_state_from(
    graphql: &str,
    node_did: &str,
    client_did: &str,
) -> anyhow::Result<InferenceSetupState> {
    use anyhow::Context;
    use gents::config_client::GraphqlEndpoint;
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
    let endpoint = GraphqlEndpoint::as_principal(graphql, client_did);
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
    use serde_json::{json, Value};
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
            identity.did(),
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
            gents::identity::defradb_bearer_authorization(node, "localhost").is_err(),
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
                identity.did()
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
                identity.did(),
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
            identity.did(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("response limit"));
    }

    #[tokio::test]
    async fn inference_metadata_requires_complete_scoped_configuration() {
        let node = "did:runtime";
        let data = json!({ "nodes": [{"node_did": node}], "agents": [], "inference_profiles": [], "inference_backends": [] });
        for (name, payload, accepted) in [
            ("complete", data.clone(), true),
            (
                "missing node",
                {
                    let mut data = data.clone();
                    data["nodes"] = json!([]);
                    data
                },
                false,
            ),
            (
                "duplicate node",
                {
                    let mut data = data.clone();
                    data["nodes"] = json!([{"node_did":node},{"node_did":node}]);
                    data
                },
                false,
            ),
            (
                "malformed agent",
                {
                    let mut data = data.clone();
                    data["agents"] = json!([{"agent_id":"incomplete"}]);
                    data
                },
                false,
            ),
            (
                "missing root",
                {
                    let mut data = data.clone();
                    data.as_object_mut().unwrap().remove("inference_backends");
                    data
                },
                false,
            ),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v0/graphql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": payload})))
                .expect(1)
                .mount(&server)
                .await;
            let access = gents::config_client::ConfigAccess::graphql(format!(
                "{}/api/v0/graphql",
                server.uri()
            ));
            assert_eq!(
                load_inference_setup_state(&access, node).await.is_ok(),
                accepted,
                "{name}"
            );
            let request: Value =
                serde_json::from_slice(&server.received_requests().await.unwrap()[0].body).unwrap();
            let query = request["query"].as_str().unwrap();
            assert!(query.contains("inference_backends:"));
            assert!(!query.contains("requests:"));
            assert!(!query.contains("sessions:"));
        }
    }

    #[test]
    fn inference_metadata_never_exports_backend_credentials() {
        let backend: gents::document_config::InferenceBackend = serde_json::from_value(json!({
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
