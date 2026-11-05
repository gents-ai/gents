use serde_json::Value;
use std::time::Duration;

use super::{graphql, ConfigClientError, GraphqlEndpoint, Result};

async fn goal_delete_snapshot(
    txn: &super::ConfigApplyTxn<'_>,
    doc_id: &str,
) -> Result<db::block::builder::ClientUpdateSnapshot> {
    let history = defra_http::client_signed_update::history_request(doc_id);
    let response = txn
        .execute_with_variables(
            &history.query,
            &history.variables.unwrap_or_else(|| serde_json::json!({})),
        )
        .await?;
    let response: query::QueryResponse =
        serde_json::from_value(response).map_err(ConfigClientError::ClientSignedSnapshotDecode)?;
    defra_http::client_signed_update::snapshot_from_response(doc_id, &response)
        .map_err(ConfigClientError::ClientConfigurationGraph)
}

pub(crate) async fn prepare_goal_delete_evidence(
    access: &super::ConfigAccess,
    signer: &dyn crate::store::identity::NodeIdentity,
    doc_id: &str,
) -> Result<Value> {
    let capability = signer.client_signing_capability()?;
    if capability.did() != signer.did() || access.actor_did().as_deref() != Some(signer.did()) {
        return Err(ConfigClientError::ClientSignerMismatch);
    }
    let snapshot = access
        .transact_readonly("goal.prepare_delete_graph", |txn| {
            Box::pin(async move { goal_delete_snapshot(txn, doc_id).await })
        })
        .await?;
    let doc_id = doc_id.to_owned();
    let signing = (*capability.config).clone();
    tokio::task::spawn_blocking(move || {
        let schema = signing_schema(gents_protocol::schemas::GOAL, "Goal")?;
        let graph = db::block::builder::ClientDeleteSignatures::from_snapshot(
            &schema, &doc_id, &snapshot, &signing,
        )
        .map_err(ConfigClientError::ClientConfigurationDeleteGraph)?;
        Ok(serde_json::to_value(graph.evidence())?)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)?
}

pub(crate) async fn verify_goal_delete_evidence(
    txn: &super::ConfigApplyTxn<'_>,
    doc_id: &str,
    evidence: Value,
) -> Result<db::block::builder::VerifiedClientMutation> {
    let snapshot = goal_delete_snapshot(txn, doc_id).await?;
    let doc_id = doc_id.to_owned();
    tokio::task::spawn_blocking(move || {
        let schema = signing_schema(gents_protocol::schemas::GOAL, "Goal")?;
        let evidence = serde_json::from_value(evidence)?;
        db::block::builder::VerifiedClientMutation::verify_delete(
            &schema, &doc_id, &snapshot, evidence,
        )
        .map_err(ConfigClientError::ClientVerifiedMutationGraph)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)?
}

/// An explicitly held client key for HTTP bearer and native block signing.
/// The secret is shared only within this process and is redacted from Debug.
#[derive(Clone)]
pub struct ClientSigningCapability {
    did: String,
    pub(super) config: std::sync::Arc<defra_core::signing::SigningConfig>,
}

impl ClientSigningCapability {
    pub fn from_ed25519_key(private_key: &[u8]) -> Result<Self> {
        use identity::Identity as _;
        let raw = identity::RawIdentity::from_bytes(crypto::KeyType::Ed25519, private_key)
            .map_err(crate::store::identity::IdentityError::LoadNodeSigning)?;
        let did = raw
            .did()
            .map_err(crate::store::identity::IdentityError::from)?
            .to_string();
        let public_key_bytes = raw.public_key_bytes();
        let config = defra_core::signing::SigningConfig {
            key_type: defra_core::signing::SigningKeyType::Ed25519,
            private_key_bytes: defra_core::signing::SigningConfig::private_key_bytes_from_slice(
                private_key,
            ),
            public_key_hex: crate::store::identity::commit_signer_identity_for_did(&did)?,
            public_key_bytes,
            remote_signer: None,
            signing_authorization: None,
        };
        Ok(Self {
            did,
            config: std::sync::Arc::new(config),
        })
    }

    pub fn did(&self) -> &str {
        &self.did
    }
}

impl std::fmt::Debug for ClientSigningCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientSigningCapability")
            .field("did", &self.did)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ClientSigningCapability {
    fn eq(&self, other: &Self) -> bool {
        self.did == other.did
    }
}
impl Eq for ClientSigningCapability {}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CreateRoute {
    Native,
    Signed,
    Refuse,
}

pub(super) fn create_route(intent: bool, agent: bool, create: bool, http: bool) -> CreateRoute {
    if !intent || !agent || !create {
        CreateRoute::Native
    } else if http {
        CreateRoute::Signed
    } else {
        CreateRoute::Refuse
    }
}

pub(super) fn trusted_create_schema(collection: crate::Collection) -> Option<&'static str> {
    gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
        .find_map(|(name, schema)| (*name == collection.graphql_type()).then_some(*schema))
}

pub(super) fn trusted_delete_schema(collection: &str) -> Option<&'static str> {
    if ![
        gents_protocol::schemas::WORKSPACE_ROOT_NAME,
        gents_protocol::schemas::PACK_INSTALLATION_NAME,
        gents_protocol::schemas::BACKEND_CREDENTIAL_NAME,
        gents_protocol::schemas::INFERENCE_BACKEND_OBSERVATION_NAME,
        gents_protocol::schemas::GOAL_NAME,
        gents_protocol::schemas::GOAL_CREATION_CLAIM_NAME,
    ]
    .contains(&collection)
        && !crate::Collection::ALL
            .iter()
            .any(|item| item.graphql_type() == collection)
    {
        return None;
    }
    gents_protocol::schemas::RUNTIME_COLLECTION_NAMES
        .iter()
        .zip(gents_protocol::schemas::RUNTIME_ALL)
        .chain(
            gents_protocol::schemas::ALL_COLLECTION_NAMES
                .iter()
                .zip(gents_protocol::schemas::ALL),
        )
        .find_map(|(name, schema)| (*name == collection).then_some(*schema))
}

/// Canonical migrated collections retain their root identity and pinned target
/// version. Only the exact bundled SDL is eligible for that local lineage;
/// server-provided schemas and versions cannot authorize a signing graph.
pub(super) fn signing_schema(sdl: &str, collection: &str) -> Result<::schema::CollectionVersion> {
    let mut schemas = query::parse_sdl(sdl)?;
    if schemas.len() != 1 || schemas[0].name != collection {
        return Err(ConfigClientError::ClientAgentTransportRequired);
    }
    let mut schema = schemas.remove(0);
    let bundled = crate::Collection::ALL
        .iter()
        .copied()
        .find(|item| item.graphql_type() == collection)
        .and_then(trusted_create_schema)
        .or_else(|| trusted_delete_schema(collection))
        .or_else(|| match collection {
            gents_protocol::schemas::AGENT_REQUEST_NAME => {
                Some(gents_protocol::schemas::AGENT_REQUEST)
            }
            gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL_NAME => {
                Some(gents_protocol::schemas::AGENT_SESSION_GOAL_CONTROL)
            }
            _ => None,
        });
    if bundled == Some(sdl) {
        if let Some(plan) = gents_migration::canonical_schema_plan(collection)
            .map_err(ConfigClientError::ClientCanonicalSchema)?
        {
            let mut root = query::parse_sdl(plan.root_sdl)?;
            if root.len() != 1
                || root[0].name != collection
                || root[0].version_id != plan.root_version
            {
                return Err(ConfigClientError::ClientAgentTransportRequired);
            }
            schema.collection_id = root.remove(0).collection_id;
            schema.version_id = plan.version;
        }
    }
    Ok(schema)
}

pub(super) fn update_route(intent: bool, collection: crate::Collection, http: bool) -> CreateRoute {
    if !intent {
        CreateRoute::Native
    } else if http && trusted_create_schema(collection).is_some() {
        CreateRoute::Signed
    } else {
        CreateRoute::Refuse
    }
}

pub(super) fn ordinary_mutation_allowed(intent: bool, mutation: bool) -> bool {
    !intent || !mutation
}

pub(super) fn configuration_replacement_patch(
    collection: crate::Collection,
    current: &Value,
    proposed: &Value,
) -> Result<Value> {
    let schema =
        trusted_create_schema(collection).ok_or(ConfigClientError::ClientAgentTransportRequired)?;
    let schemas = query::parse_sdl(schema)?;
    let schema = schemas
        .first()
        .ok_or(ConfigClientError::ClientAgentTransportRequired)?;
    let mut patch = proposed.clone();
    let object = patch
        .as_object_mut()
        .ok_or(ConfigClientError::ClientConfigurationUpdateInvalid)?;
    for field in schema.fields.iter().filter(|field| field.immutable) {
        if let Some(value) = object.get(&field.name) {
            if current.get(&field.name) != Some(value) {
                return Err(ConfigClientError::ClientConfigurationImmutableField {
                    field: field.name.clone(),
                });
            }
            object.remove(&field.name);
        }
    }
    Ok(patch)
}

pub(super) fn update_target(mutation: &str, variables: &Value, collection: &str) -> Result<String> {
    let variables = variables
        .as_object()
        .ok_or(ConfigClientError::VariablesNotObject)?;
    let variables = variables
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let parsed = query::query_parse::parse_request_with_limits(
        mutation,
        Some(&variables),
        None,
        query::QueryLimits::default(),
    )?;
    let query::query_parse::ParsedOperation::Mutation {
        mutations,
        explain: None,
    } = parsed
    else {
        return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
    };
    if mutations.len() != 1
        || mutations[0].mutation_type != query::MutationType::Update
        || mutations[0].collection_name != collection
        || mutations[0].encryption().is_some()
    {
        return Err(ConfigClientError::ClientConfigurationUpdateInvalid);
    }
    defra_http::client_signed_update::single_document_target(&mutations[0])
        .map(str::to_owned)
        .ok_or(ConfigClientError::ClientConfigurationUpdateInvalid)
}

pub(super) fn client_signer(
    endpoint: &GraphqlEndpoint,
) -> Result<defra_core::signing::SigningConfig> {
    let capability = endpoint
        .client_signing_capability
        .as_ref()
        .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
    if endpoint.delegated_authorization.is_some()
        || endpoint.principal_did.as_deref() != Some(capability.did())
    {
        return Err(ConfigClientError::ClientSignerMismatch);
    }
    Ok((*capability.config).clone())
}

pub(super) async fn execute(
    endpoint: &GraphqlEndpoint,
    txn_id: &str,
    mutation: &str,
    variables: &Value,
    schema: String,
    collection: String,
) -> Result<Value> {
    let signer = client_signer(endpoint)?;
    let input = variables
        .get("input")
        .cloned()
        .ok_or(ConfigClientError::VariablesNotObject)?;
    let mut graph = tokio::task::spawn_blocking(move || {
        let schema = signing_schema(&schema, &collection)?;
        db::block::builder::ClientCreateSignatures::from_input(
            &storage::Crdts::default(),
            &schema,
            &input,
            &signer,
        )
        .map_err(ConfigClientError::ClientAgentGraph)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)??;
    let mut url =
        reqwest::Url::parse(endpoint.url()).map_err(|source| ConfigClientError::ParseEndpoint {
            url: endpoint.url().to_owned(),
            source,
        })?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(ConfigClientError::ClientAgentTransportRequired),
    };
    if !url.path().ends_with("/graphql") || url.set_scheme(scheme).is_err() {
        return Err(ConfigClientError::ClientAgentTransportRequired);
    }
    url.set_path(&format!("{}/client-sign", url.path()));
    let authorization = endpoint
        .authorization()?
        .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
    let request = defra_http::client_signed_create::websocket_request(url.as_str(), &authorization)
        .map_err(ConfigClientError::ClientAgentSigning)?;
    let query = query::QueryRequest::new(mutation).with_variables(variables.clone());
    let response = tokio::time::timeout(Duration::from_secs(90), async {
        let (socket, _) = tokio_tungstenite::connect_async_with_config(
            request,
            Some(defra_http::client_signed_create::websocket_config()),
            false,
        )
        .await
        .map_err(|error| ConfigClientError::ClientAgentConnection(Box::new(error)))?;
        defra_http::client_signed_create::execute_with_socket(
            socket,
            query,
            Some(txn_id),
            &mut graph,
        )
        .await
        .map_err(ConfigClientError::ClientAgentSigning)
    })
    .await
    .map_err(ConfigClientError::ClientAgentTimeout)??;
    graphql::validate_txn_response(serde_json::to_value(response)?)
}

/// Execute one client-approved update in the caller's existing transaction.
pub(super) async fn execute_update(
    endpoint: &GraphqlEndpoint,
    txn_id: &str,
    mutation: &str,
    variables: &Value,
    schema: String,
    collection: String,
    doc_id: String,
    snapshot: db::block::builder::ClientUpdateSnapshot,
) -> Result<Value> {
    let signer = client_signer(endpoint)?;
    let input = variables
        .get("input")
        .cloned()
        .ok_or(ConfigClientError::VariablesNotObject)?;
    let mut graph = tokio::task::spawn_blocking(move || {
        let schema = signing_schema(&schema, &collection)?;
        db::block::builder::ClientUpdateSignatures::from_input(
            &storage::Crdts::default(),
            &schema,
            &doc_id,
            &input,
            &snapshot,
            &signer,
        )
        .map_err(ConfigClientError::ClientConfigurationGraph)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)??;
    let mut url =
        reqwest::Url::parse(endpoint.url()).map_err(|source| ConfigClientError::ParseEndpoint {
            url: endpoint.url().to_owned(),
            source,
        })?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(ConfigClientError::ClientAgentTransportRequired),
    };
    if !url.path().ends_with("/graphql") || url.set_scheme(scheme).is_err() {
        return Err(ConfigClientError::ClientAgentTransportRequired);
    }
    url.set_path(&format!("{}/client-sign", url.path()));
    let authorization = endpoint
        .authorization()?
        .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
    let request = defra_http::client_signed_create::websocket_request(url.as_str(), &authorization)
        .map_err(ConfigClientError::ClientAgentSigning)?;
    let query = query::QueryRequest::new(mutation).with_variables(variables.clone());
    let response = tokio::time::timeout(Duration::from_secs(90), async {
        let (socket, _) = tokio_tungstenite::connect_async_with_config(
            request,
            Some(defra_http::client_signed_create::websocket_config()),
            false,
        )
        .await
        .map_err(|error| ConfigClientError::ClientAgentConnection(Box::new(error)))?;
        defra_http::client_signed_update::execute_with_socket(
            socket,
            query,
            Some(txn_id),
            &mut graph,
        )
        .await
        .map_err(ConfigClientError::ClientAgentSigning)
    })
    .await
    .map_err(ConfigClientError::ClientAgentTimeout)??;
    graphql::validate_txn_response(serde_json::to_value(response)?)
}

/// Execute one client-approved physical deletion in the caller's transaction.
pub(super) async fn execute_delete(
    endpoint: &GraphqlEndpoint,
    txn_id: &str,
    mutation: &str,
    schema: String,
    collection: String,
    doc_id: String,
    snapshot: db::block::builder::ClientUpdateSnapshot,
) -> Result<Value> {
    let signer = client_signer(endpoint)?;
    let mut graph = tokio::task::spawn_blocking(move || {
        let schema = signing_schema(&schema, &collection)?;
        db::block::builder::ClientDeleteSignatures::from_snapshot(
            &schema, &doc_id, &snapshot, &signer,
        )
        .map_err(ConfigClientError::ClientConfigurationDeleteGraph)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)??;
    let mut url =
        reqwest::Url::parse(endpoint.url()).map_err(|source| ConfigClientError::ParseEndpoint {
            url: endpoint.url().to_owned(),
            source,
        })?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(ConfigClientError::ClientAgentTransportRequired),
    };
    if !url.path().ends_with("/graphql") || url.set_scheme(scheme).is_err() {
        return Err(ConfigClientError::ClientAgentTransportRequired);
    }
    url.set_path(&format!("{}/client-sign", url.path()));
    let authorization = endpoint
        .authorization()?
        .ok_or(ConfigClientError::ClientAgentSignerRequired)?;
    let request = defra_http::client_signed_create::websocket_request(url.as_str(), &authorization)
        .map_err(ConfigClientError::ClientAgentSigning)?;
    let query = query::QueryRequest::new(mutation);
    let response = tokio::time::timeout(Duration::from_secs(90), async {
        let (socket, _) = tokio_tungstenite::connect_async_with_config(
            request,
            Some(defra_http::client_signed_create::websocket_config()),
            false,
        )
        .await
        .map_err(|error| ConfigClientError::ClientAgentConnection(Box::new(error)))?;
        defra_http::client_signed_delete::execute_with_socket(
            socket,
            query,
            Some(txn_id),
            &mut graph,
        )
        .await
        .map_err(ConfigClientError::ClientAgentSigning)
    })
    .await
    .map_err(ConfigClientError::ClientAgentTimeout)??;
    graphql::validate_txn_response(serde_json::to_value(response)?)
}
pub(crate) async fn prepare_request_evidence(
    signer: &dyn crate::store::identity::NodeIdentity,
    input: Value,
) -> Result<Value> {
    let capability = signer.client_signing_capability()?;
    if capability.did() != signer.did() {
        return Err(ConfigClientError::ClientSignerMismatch);
    }
    let signing = (*capability.config).clone();
    tokio::task::spawn_blocking(move || {
        let schema = signing_schema(gents_protocol::schemas::AGENT_REQUEST, "AgentRequest")?;
        let graph = db::block::builder::ClientCreateSignatures::from_input(
            &storage::Crdts::default(),
            &schema,
            &input,
            &signing,
        )
        .map_err(ConfigClientError::ClientAgentGraph)?;
        Ok(serde_json::to_value(graph.evidence())?)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)?
}

pub(crate) async fn verify_request_evidence(
    input: Value,
    evidence: Value,
) -> Result<db::block::builder::VerifiedClientCreate> {
    tokio::task::spawn_blocking(move || {
        let schema = signing_schema(gents_protocol::schemas::AGENT_REQUEST, "AgentRequest")?;
        let evidence = serde_json::from_value(evidence)?;
        db::block::builder::VerifiedClientCreate::verify(
            &storage::Crdts::default(),
            &schema,
            &input,
            evidence,
        )
        .map_err(ConfigClientError::ClientAgentGraph)
    })
    .await
    .map_err(ConfigClientError::ClientAgentGraphJoin)?
}
