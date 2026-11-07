use serde_json::Value;

pub(crate) fn node_builder() -> defra_node::NodeBuilder {
    use gents::NodeIdentity as _;

    let directory = tempfile::tempdir().unwrap();
    let identity =
        gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap();
    defra_node::Node::builder().with_node_identity_did(identity.did())
}

/// A node whose runtime is the deterministic fixture signer
/// (`caused_sessions::fixtures::FIXTURE_DID`), whose key this process holds.
pub(crate) fn fixture_node_builder() -> defra_node::NodeBuilder {
    use gents::NodeIdentity as _;

    let signer = crate::caused_sessions::fixtures::fixture_signer();
    defra_node::Node::builder().with_node_identity_did(signer.did())
}

/// The fixture signer's managed Agent for one test: derived from the held
/// runtime root, so its signing key is registered in this process.
pub(crate) fn fixture_agent() -> gents_protocol::agent_identity::AgentIdentityBinding {
    gents::store::identity::provision_agent_identity(
        &crate::caused_sessions::fixtures::fixture_signer(),
    )
    .unwrap()
}

/// The fixture signer's managed Agent named `name`: the same name derives the
/// same Agent DID, so fixtures can name Agents across documents and reads.
pub(crate) fn fixture_agent_named(
    name: &str,
) -> gents_protocol::agent_identity::AgentIdentityBinding {
    use sha2::Digest as _;

    let creation_nonce = sha2::Sha256::digest(name.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let identity = gents::store::identity::derive_agent_identity(
        &crate::caused_sessions::fixtures::fixture_signer(),
        &creation_nonce,
    )
    .unwrap();
    gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: identity.did().to_owned(),
        creation_nonce,
    }
}

/// Publishes the fixture Node for `node`'s runtime as that runtime: local
/// writes are admitted only for the runtime or a managed Agent, and runtime
/// startup requires the Node to exist. Returns the Node's DID.
pub(crate) async fn publish_test_node(node: &std::sync::Arc<defra_node::Node>) -> String {
    let node_did = node
        .node_identity_did()
        .expect("fixture node has a runtime identity")
        .to_owned();
    publish_test_node_as(node, &node_did).await;
    node_did
}

/// [`publish_test_node`] for a fixture that names its Node's DID; the DID
/// must be the node's runtime.
pub(crate) async fn publish_test_node_as(node: &std::sync::Arc<defra_node::Node>, node_did: &str) {
    use gents::store::config_client::{
        apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    assert_eq!(
        node.node_identity_did(),
        Some(node_did),
        "the fixture Node is published by its own runtime"
    );
    if gents::load_node(node, node_did).await.unwrap().is_some() {
        return;
    }
    let value = serde_json::json!({"node_did": node_did});
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents::Collection::Node,
        add: value.clone(),
        update: value,
    }])
    .unwrap();
    ConfigAccess::local_as(node.clone(), ::identity::Did::new(node_did).unwrap())
        .transact_write("test.publish_node", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
}

/// A fixture node served over its own DefraDB HTTP endpoint with node ACP, so
/// a User's client-signed writes reach it as they do from the CLI. Returns
/// the node and its GraphQL URL.
pub(crate) async fn served_node(
    builder: defra_node::NodeBuilder,
) -> (std::sync::Arc<defra_node::Node>, String) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let graphql = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let node = builder
        .with_node_acp_enabled()
        .with_http(gents::defra_node::HttpConfig::with_listener(listener).unwrap())
        .build()
        .await
        .unwrap();
    (std::sync::Arc::new(node), graphql)
}

/// The User client's access over `graphql`, signed by its own key as the
/// CLI's endpoint owner signs it.
pub(crate) fn user_access(
    graphql: &str,
    user: &dyn gents::NodeIdentity,
) -> gents::store::config_client::ConfigAccess {
    gents::store::config_client::ConfigAccess::Graphql(
        gents::store::config_client::GraphqlEndpoint::as_principal(graphql, user.did())
            .with_client_signing_capability(user.client_signing_capability().unwrap())
            .unwrap()
            .with_client_signed_agent_creates(),
    )
}

/// Seeds an xAI backend whose profile effort will not be sent, as the
/// fixture runtime. Returns the runtime's access and its Node DID.
pub(crate) async fn seed_unsent_xai_effort() -> (gents::store::config_client::ConfigAccess, String)
{
    let node = std::sync::Arc::new(fixture_node_builder().build().await.unwrap());
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let owner = publish_test_node(&node).await;
    let access = gents::store::config_client::ConfigAccess::local_as(
        node,
        ::identity::Did::new(owner.as_str()).unwrap(),
    );
    let owner = owner.as_str();
    let mut value = xai_effort_backend(owner);
    let catalogs = value.as_object_mut().unwrap().remove("catalogs").unwrap();
    value.as_object_mut().unwrap().remove("probe_status");
    let backend: gents::InferenceBackend = serde_json::from_value(value).unwrap();
    gents::store::config_client::write_inference_backend_document(&access, &backend)
        .await
        .unwrap();
    let input = gents_protocol::graphql::graphql_input_literal(&xai_effort_profile(owner)).unwrap();
    access
        .write(
            "test.profile.seed",
            &format!("mutation {{ create_InferenceProfile(input: {input}) {{_docID}} }}"),
        )
        .await
        .unwrap();
    let catalog: gents::documents::BackendModelCatalog =
        serde_json::from_value(catalogs["entries"][0].clone()).unwrap();
    access
        .transact_write("test.backend.catalog", |txn| {
            let backend = &backend;
            let catalog = catalog.clone();
            Box::pin(async move {
                gents::inference::backends::registry::record_model_catalog_in_txn(
                    txn, backend, catalog,
                )
                .await
            })
        })
        .await
        .unwrap();
    gents::inference::backends::registry::set_backend_probe_status_as(
        &access,
        owner,
        &backend.backend_id,
        "unhealthy",
    )
    .await
    .unwrap();
    let owner = owner.to_owned();
    (access, owner)
}

pub(crate) fn xai_effort_backend(owner: &str) -> Value {
    serde_json::json!({
        "node_did": owner, "backend_id": "xai", "name": "xAI",
        "provider_kind": "OpenAiCompatible", "openai_wire_api": "responses",
        "endpoint": "https://api.x.ai/v1", "enabled": true,
        "auth": {"kind": "environment", "variable": "XAI_API_KEY"},
        "probe_status": "unhealthy",
        "catalogs": {"entries": [{"node_did": null, "observed_at": "2026-01-01T00:00:00Z",
            "models": [{"model_name": "grok-4.20-0309-reasoning", "reasoning_efforts": []}]}]}
    })
}

pub(crate) fn xai_effort_profile(owner: &str) -> Value {
    serde_json::json!({
        "node_did": owner, "profile_id": "grok", "backend_id": "xai",
        "model_name": "grok-4.20-0309-reasoning", "reasoning_effort": "low"
    })
}

#[tokio::test]
async fn catalog_freshness_reaches_cli_observation_reader() -> anyhow::Result<()> {
    let node = std::sync::Arc::new(fixture_node_builder().build().await?);
    gents::ensure_runtime_schemas(&node).await?;
    let owner = publish_test_node(&node).await;
    let backend: gents::documents::InferenceBackend = serde_json::from_value(serde_json::json!({
        "node_did":owner, "backend_id":"catalog-backend", "name":"Catalog",
        "provider_kind":"OpenAiCompatible", "endpoint":"http://127.0.0.1:1/v1",
        "auth":{"kind":"unauthenticated"}
    }))?;
    let access = gents::ConfigAccess::local_as(node.clone(), ::identity::Did::new(owner)?);
    gents::store::config_client::write_inference_backend_document(&access, &backend).await?;
    for observed_at in ["2026-10-28T01:00:00Z", "2026-10-28T02:00:00Z"] {
        let catalog: gents::documents::BackendModelCatalog =
            serde_json::from_value(serde_json::json!({
                "node_did":null, "observed_at":observed_at,
                "models":[{"model_name":"retained-model"}]
            }))?;
        access
            .transact_write("test.catalog.refresh", |txn| {
                let backend = &backend;
                let catalog = catalog.clone();
                Box::pin(async move {
                    gents::inference::backends::registry::record_model_catalog_in_txn(
                        txn, backend, catalog,
                    )
                    .await
                })
            })
            .await?;
    }
    let observation =
        super::load_backend_observation(&access, &backend.node_did, &backend.backend_id).await?;
    assert_eq!(observation.catalogs[0].observed_at, "2026-10-28T02:00:00Z");
    assert_eq!(
        observation.catalogs[0].models[0].model_name,
        "retained-model"
    );
    node.shutdown().await.expect("node shutdown");
    Ok(())
}

/// A served home whose enrolled User configures it: the User holds the
/// operator grants, publishes the Node through its client-signed endpoint, and
/// creates managed Agents that sign their own sessions and requests.
pub(crate) struct ClientHome {
    pub(crate) home: gents::eval::runner::embedded::EmbeddedHome,
    pub(crate) user: gents::KeyIdentity,
    /// The User's client-signed access.
    pub(crate) access: gents::store::config_client::ConfigAccess,
    _user_dir: tempfile::TempDir,
}

impl ClientHome {
    pub(crate) async fn new() -> Self {
        use gents::store::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use gents::NodeIdentity as _;

        let home = gents::eval::runner::embedded::EmbeddedHome::create_temp("cli-client-home")
            .await
            .unwrap();
        let user_dir = tempfile::tempdir().unwrap();
        let user = gents::KeyIdentity::load_or_create(user_dir.path().join("user.key"), None)
            .unwrap();
        gents::store::identity::GrantSet::OperatorUser
            .grant(
                &gents::store::config_client::ConfigAccess::local_as(
                    home.node.clone(),
                    ::identity::Did::new(home.did()).unwrap(),
                ),
                &::identity::Did::new(user.did()).unwrap(),
            )
            .await
            .unwrap();
        let access = user_access(home.graphql_endpoint().unwrap(), &user);
        let node = serde_json::json!({"node_did": home.did()});
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: gents::Collection::Node,
            add: node.clone(),
            update: node,
        }])
        .unwrap();
        access
            .transact_write("test.client_home.node", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
        Self {
            home,
            user,
            access,
            _user_dir: user_dir,
        }
    }

    pub(crate) fn node(&self) -> &std::sync::Arc<defra_node::Node> {
        &self.home.node
    }

    pub(crate) fn node_did(&self) -> &str {
        self.home.did()
    }

    pub(crate) fn graphql(&self) -> &str {
        self.home.graphql_endpoint().unwrap()
    }

    /// The node's runtime access, for runtime-owned state and native grants.
    pub(crate) fn runtime(&self) -> gents::store::config_client::ConfigAccess {
        gents::store::config_client::ConfigAccess::local_as(
            self.home.node.clone(),
            ::identity::Did::new(self.home.did()).unwrap(),
        )
    }

    /// Makes the User the household owner `gents init` creates, the native
    /// admin that installs packs and their collection schemas.
    pub(crate) async fn grant_household_owner(&self) {
        use gents::NodeIdentity as _;
        gents::store::identity::GrantSet::HouseholdOwner
            .grant(
                &self.runtime(),
                &::identity::Did::new(self.user.did()).unwrap(),
            )
            .await
            .unwrap();
    }

    /// Provisions a managed Agent and publishes it, with a placeholder
    /// backend and profile, as the User.
    pub(crate) async fn agent(&self, display_name: &str) -> ManagedAgent {
        use gents::store::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use gents::NodeIdentity as _;

        let runtime = self.home.identity.as_ref();
        let binding = gents::store::identity::provision_agent_identity_authorized(
            self.node(),
            runtime,
            &::identity::Did::new(self.user.did()).unwrap(),
        )
        .await
        .unwrap();
        let identity = gents::store::identity::resolve_agent_identity(runtime, &binding).unwrap();
        let backend_id = format!("{}:backend", binding.agent_did);
        let profile_id = format!("{}:inference", binding.agent_did);
        let documents = [
            (
                gents::Collection::InferenceBackend,
                serde_json::json!({
                    "node_did": runtime.did(), "backend_id": backend_id, "name": "Test inference",
                    "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                    "auth": {"kind": "unauthenticated"},
                }),
            ),
            (
                gents::Collection::InferenceProfile,
                serde_json::json!({
                    "node_did": runtime.did(), "profile_id": profile_id,
                    "backend_id": backend_id, "model_name": "test-model",
                }),
            ),
            (
                gents::Collection::Agent,
                serde_json::json!({
                    "node_did": runtime.did(), "agent_did": binding.agent_did,
                    "creation_nonce": binding.creation_nonce, "display_name": display_name,
                    "inference_profile_id": profile_id, "enabled": true,
                }),
            ),
        ];
        let plan = DesiredStateApplyPlan::new(
            documents
                .into_iter()
                .map(|(collection, value)| DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                })
                .collect(),
        )
        .unwrap();
        self.access
            .transact_write("test.client_home.agent", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap();
        let access = gents::store::config_client::ConfigAccess::local_managed_agent(
            self.node().clone(),
            identity.did().parse().unwrap(),
            identity.as_ref(),
        )
        .unwrap();
        ManagedAgent {
            identity,
            binding,
            access,
            profile_id,
            backend_id,
        }
    }
}

/// A managed Agent of a [`ClientHome`] and its own signed access.
pub(crate) struct ManagedAgent {
    pub(crate) identity: std::sync::Arc<dyn gents::NodeIdentity>,
    pub(crate) binding: gents_protocol::agent_identity::AgentIdentityBinding,
    pub(crate) access: gents::store::config_client::ConfigAccess,
    pub(crate) profile_id: String,
    pub(crate) backend_id: String,
}

impl ManagedAgent {
    pub(crate) fn did(&self) -> &str {
        &self.binding.agent_did
    }
}
