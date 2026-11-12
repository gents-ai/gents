use std::collections::BTreeSet;
use std::sync::Arc;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use gents_loop::tool::ToolDyn;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_tools::surface::SelfConfigToolConfig;
use gents_protocol::tool_names::CONFIG_TOOL_NAME;
use gents_tools::builtin::self_config::*;

pub fn test_plugins() -> Arc<crate::plugin::executor::PluginExecutor> {
    Arc::new(crate::plugin::executor::PluginExecutor::default())
}

pub fn config(categories: &[&str]) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        agent_did: "beh-test".to_string(),
        categories: categories.iter().map(|c| c.to_string()).collect(),
        no_lockout: false,
        preview: false,
        enable_pack_install: false,
        enable_pack_authority: false,
        process_ceiling: Default::default(),
        selected_plugins: Vec::new(),
        ceiling_plugins: None,
    }
}

/// Grants `subject` the direct sets Lean `Identity.GrantSets.directSets` gives
/// `recipient`, as the runtime owner does at init. The fixture Agent is its
/// Node's default Agent, so it holds the modeled default-Agent sets.

pub async fn grant_modeled_direct_sets(
    db: &gents_inference::test_support::TestDb,
    recipient: &str,
    subject: &str,
) {
    use gents_store::identity::GrantSet;
    let wanted = recipient.to_owned();
    let case = tokio::task::spawn_blocking(move || {
        gents_lean_vocab::lean_contract_snapshot()
            .identity_grant_recipient_cases
            .iter()
            .find(|case| case.recipient == wanted)
            .cloned()
    })
    .await
    .unwrap()
    .unwrap_or_else(|| panic!("Lean recipient {recipient:?}"));
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let subject = identity::Did::new(subject).unwrap();
    for name in &case.direct {
        let set = GrantSet::ALL
            .into_iter()
            .find(|set| set.name() == name)
            .unwrap_or_else(|| panic!("Lean grant set {name:?} has no Rust owner"));
        set.grant(&admin, &subject).await.unwrap();
    }
}

/// Grants `subject` the node permissions the init Engineer holds through the
/// tool sources of its preset Tools: every self-config category plus pack
/// installation, and a datastore query surface (Lean
/// `Identity.Configuration.toolOperationSources`, cases `self-config-all`,
/// `schema-and-pack-overlap` without schema, and `datastore-query`). A fixture
/// Agent that authors its own Engineer-like Tools must already hold these: the
/// native held-bounded rule refuses a self-granted tool source.

pub async fn grant_engineer_tool_source_permissions(
    db: &gents_inference::test_support::TestDb,
    subject: &str,
) {
    use gents_store::config_client::NodePermission;
    let permissions = tokio::task::spawn_blocking(|| {
        let model = gents_lean_vocab::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("configuration contract");
        let mut permissions = std::collections::BTreeSet::new();
        for name in ["self-config-all", "datastore-query"] {
            let case = model
                .tool_operation_cases
                .iter()
                .find(|case| case["name"] == name)
                .unwrap_or_else(|| panic!("modeled operation case {name}"));
            for source in case["sources"].as_array().unwrap() {
                permissions.insert(source["permission"].as_str().unwrap().to_owned());
            }
        }
        permissions
    })
    .await
    .unwrap();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let subject = identity::Did::new(subject).unwrap();
    for name in permissions {
        let permission = NodePermission::parse(&name)
            .unwrap_or_else(|| panic!("modeled permission {name:?} has no native owner"));
        admin
            .grant_node_permission(&subject, permission)
            .await
            .unwrap();
    }
}

pub struct SelfConfigFixture {
    pub db: gents_inference::test_support::TestDb,
    pub user_access: gents_store::config_client::ConfigAccess,
    pub agent: Arc<dyn gents_store::identity::NodeIdentity>,
    pub agent_did: String,
    pub node_binding: gents_documents::ConfigurationNodeBinding,
}

impl SelfConfigFixture {
    pub async fn new(name: &str) -> Self {
        let (db, _user, user_access, binding) = gents_inference::test_support::signed_test_node(
            name,
            gents_session::test_support::no_lifecycle_plugins(),
        )
        .await;
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .expect("fixture Agent is derived from the held runtime");
        let response = user_access.execute("{ Node { _docID } }").await.unwrap();
        let node_binding = gents_documents::ConfigurationNodeBinding {
            node_did: identity::Did::new(db.node_identity.did()).unwrap(),
            doc_id: response["data"]["Node"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned(),
            enabled: true,
            default_agent_did: Some(binding.agent_did.clone()),
        };
        grant_modeled_direct_sets(&db, "default-agent", &binding.agent_did).await;
        Self {
            db,
            user_access,
            agent,
            agent_did: binding.agent_did,
            node_binding,
        }
    }

    /// The fixture Agent holds what the init Engineer holds through its preset
    /// Tools (`grant_engineer_tool_source_permissions`).
    pub async fn grant_engineer_tool_sources(&self) {
        grant_engineer_tool_source_permissions(&self.db, &self.agent_did).await;
    }

    pub async fn grant_selected_configuration(&self) {
        self.user_access
            .transact_write("test.self_config.grant_selected", |txn| {
                let owner = self.db.node_identity.did();
                let agent = &self.agent_did;
                Box::pin(async move {
                    gents_documents::grant_selected_self_configuration(txn, owner, agent).await
                })
            })
            .await
            .expect("User grants selected configuration to the Agent");
    }

    pub async fn install_sibling(&self, display_name: &str) -> String {
        let user = identity::Did::new(self.user_access.actor_did().unwrap()).unwrap();
        let binding = gents_store::identity::provision_agent_identity_authorized(
            &self.db.node,
            self.db.node_identity.as_ref(),
            &user,
        )
        .await
        .unwrap();
        gents_inference::test_support::install_test_agent_binding_with_access(
            self.db.node_identity.did(),
            display_name,
            &binding,
            &self.user_access,
        )
        .await;
        binding.agent_did
    }

    /// The User publishes `root` as the selected WorkspaceRoot that the Agent
    /// may use, so Tools.host.root admission has an allowed root to match.
    pub async fn publish_workspace_root(&self, root: &std::path::Path) {
        let root = std::fs::canonicalize(root).expect("canonical workspace root");
        gents_tools::surface::set_workspace_root_with_readers(
            &self.user_access,
            self.db.node_identity.did(),
            &root.to_string_lossy(),
            None,
            true,
            std::slice::from_ref(&self.agent_did),
        )
        .await
        .expect("User publishes the selected WorkspaceRoot");
    }

    /// Publishes a directory in the fixture home as this Agent's selected
    /// WorkspaceRoot. On a signed Node the WorkspaceRoot policy is always
    /// configured, so Tools with host tools need such an admitted root.
    pub async fn admitted_workspace_root(&self) -> String {
        let root = self.db.data_path().join("workspace-root");
        std::fs::create_dir_all(&root).unwrap();
        self.publish_workspace_root(&root).await;
        std::fs::canonicalize(&root)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    pub async fn grant_node_reader(&self) {
        self.user_access
            .add_actor_relationship(
                "Node",
                &self.node_binding.doc_id,
                "runtime_reader",
                &identity::Did::new(&self.agent_did).unwrap(),
            )
            .await
            .unwrap();
    }

    /// Lean `SelfConfig.ActorGrants.catalog`: the Node's owner gives this
    /// Agent `nodes#agent_manager`, which is `manage_agents` on the Node.
    pub async fn grant_agent_catalog(&self) {
        self.user_access
            .add_actor_relationship(
                "Node",
                &self.node_binding.doc_id,
                "agent_manager",
                &identity::Did::new(&self.agent_did).unwrap(),
            )
            .await
            .unwrap();
    }

    pub async fn grant_account_fixture_selections(
        &self,
        documents: &[(gents_store::collection::Collection, String, bool)],
    ) {
        self.user_access
            .transact_write("test.account_choice.grant_selections", |txn| {
                Box::pin(async move {
                    let actor = identity::Did::new(&self.agent_did)?;
                    for (collection, id, write) in documents {
                        let (doc, _) = gents_documents::read_desired_state_record_in_txn(
                            txn,
                            *collection,
                            self.node_did(),
                            id,
                        )
                        .await?
                        .expect("account choice configuration exists");
                        txn.add_actor_relationship(
                            collection.graphql_type(),
                            &doc,
                            "selection_manager",
                            &actor,
                        )
                        .await?;
                        if *write {
                            txn.add_actor_relationship(
                                collection.graphql_type(),
                                &doc,
                                "writer",
                                &actor,
                            )
                            .await?;
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }

    pub async fn grant_selected_backend_writer(&self) {
        let backend_id = format!("{}:backend", self.agent_did());
        let response = self.user_access.execute(&format!(
            r#"{{ InferenceBackendMetadata(filter: {{backend_id: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
            escape_graphql_string(&backend_id),
        )).await.unwrap();
        let doc_id = response["data"]["InferenceBackendMetadata"][0]["_docID"]
            .as_str()
            .expect("selected backend metadata exists");
        self.user_access
            .add_actor_relationship(
                "InferenceBackendMetadata",
                doc_id,
                "writer",
                &identity::Did::new(&self.agent_did).unwrap(),
            )
            .await
            .unwrap();
    }

    pub async fn grant_agent_delete(&self) {
        gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        )
        .grant_node_permission(
            &identity::Did::new(&self.agent_did).unwrap(),
            gents_store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    }

    pub async fn grant_agent_relation_delete(&self) {
        gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        )
        .grant_node_permission(
            &identity::Did::new(&self.agent_did).unwrap(),
            gents_store::config_client::NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    }

    pub async fn grant_user_relation_delete(&self) {
        let admin = gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        );
        let user = identity::Did::new(self.user_access.actor_did().unwrap()).unwrap();
        admin
            .grant_node_permission(
                &user,
                gents_store::config_client::NodePermission::DacRelationDelete,
            )
            .await
            .unwrap();
    }

    pub async fn grant_agent_schema(&self) {
        let admin = gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        );
        let actor = identity::Did::new(&self.agent_did).unwrap();
        for permission in [
            gents_store::config_client::NodePermission::CollectionGet,
            gents_store::config_client::NodePermission::CollectionPatch,
        ] {
            admin
                .grant_node_permission(&actor, permission)
                .await
                .unwrap();
        }
    }

    pub fn schema_tool(&self) -> gents_tools::builtin::schema_help::SchemaTool {
        gents_tools::builtin::schema_help::SchemaTool::for_managed_agent(
            self.db.node.clone(),
            self.db.node_identity.did(),
            None,
            self.agent.as_ref(),
        )
        .unwrap()
    }

    pub async fn grant_sibling_configuration(&self, sibling: &str) {
        self.grant_user_relation_delete().await;
        self.user_access
            .transact_write("test.self_config.grant_sibling", |txn| {
                let owner = self.db.node_identity.did();
                let actor = &self.agent_did;
                Box::pin(async move {
                    gents_documents::grant_selected_self_configuration(txn, owner, sibling).await?;
                    let (doc_id, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        owner,
                        sibling,
                    )
                    .await?
                    .expect("sibling Agent exists");
                    txn.delete_actor_relationship(
                        "Agent",
                        &doc_id,
                        "config_writer",
                        &identity::Did::new(sibling)?,
                    )
                    .await?;
                    txn.add_actor_relationship(
                        "Agent",
                        &doc_id,
                        "config_writer",
                        &identity::Did::new(actor)?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }

    /// The User makes the Agent an `administrator` of `target`'s Agent, the
    /// relation that manages `session_writer` there.
    pub async fn grant_agent_administration(&self, target: &str) {
        self.user_access
            .transact_write("test.self_config.grant_agent_administration", |txn| {
                let owner = self.db.node_identity.did();
                let actor = &self.agent_did;
                Box::pin(async move {
                    let (doc_id, _) = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Agent,
                        owner,
                        target,
                    )
                    .await?
                    .expect("target Agent exists");
                    txn.add_actor_relationship(
                        "Agent",
                        &doc_id,
                        "administrator",
                        &identity::Did::new(actor)?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }

    pub async fn grant_pack_install(&self) {
        self.grant_pack_install_to(&self.agent_did).await;
    }

    pub async fn grant_pack_install_to(&self, agent_did: &str) {
        use gents_documents::apply_desired_state_plan;
        use gents_documents::{
            read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        let tools_id = format!("{agent_did}:tools");
        self.user_access
            .transact_write("test.self_config.grant_pack_install", |txn| {
                let tools_id = &tools_id;
                let owner = self.db.node_identity.did();
                Box::pin(async move {
                    let (_, mut tools) = read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Tools,
                        owner,
                        tools_id,
                    )
                    .await?
                    .expect("selected Tools exists");
                    tools["self_config"] =
                        json!({"enable_self_config": true, "enable_pack_install": true});
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: gents_store::collection::Collection::Tools,
                        add: tools.clone(),
                        update: tools,
                    }])?;
                    apply_desired_state_plan(
                        txn,
                        &plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }

    pub fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub fn node_did(&self) -> &str {
        self.db.node_identity.did()
    }

    /// Make the fixture User the native admin, as init makes the init User
    /// (`Identity.GrantSets.only_init_user_is_admin`).
    pub async fn grant_user_native_admin(&self) {
        let owner = gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        );
        owner
            .grant_node_admin(&identity::Did::new(self.user_access.actor_did().unwrap()).unwrap())
            .await
            .unwrap();
    }

    /// Turn native node ACP off as its owner, keeping its relationships.
    pub async fn disable_native_nac(&self) {
        let owner = defra_core::RequestContext::default()
            .with_identity(Some(self.db.node_identity.did().to_string()));
        self.db.node.disable_nac_as(&owner).await.unwrap();
    }

    /// The fixture Agent's own signed access.
    pub fn agent_access(&self) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::local_managed_agent(
            self.db.node.clone(),
            identity::Did::new(&self.agent_did).unwrap(),
            self.agent.as_ref(),
        )
        .unwrap()
    }

    pub fn user_access(&self) -> &gents_store::config_client::ConfigAccess {
        &self.user_access
    }

    pub async fn update_signed_configuration(
        &self,
        collection: gents_store::collection::Collection,
        id: &str,
        patch: Value,
    ) {
        let schema = match collection {
            gents_store::collection::Collection::AgentContext => {
                gents_protocol::schemas::AGENT_CONTEXT
            }
            gents_store::collection::Collection::InferenceProfile => {
                gents_protocol::schemas::INFERENCE_PROFILE
            }
            gents_store::collection::Collection::Node => gents_protocol::schemas::NODE,
            gents_store::collection::Collection::DefaultAgentSelection => {
                gents_protocol::schemas::DEFAULT_AGENT_SELECTION
            }
            gents_store::collection::Collection::Tools => gents_protocol::schemas::TOOLS,
            _ => panic!("no fixture schema for {}", collection.graphql_type()),
        };
        self.user_access
            .transact_write("test.self_config.signed_fixture_update", |txn| {
                let patch = &patch;
                Box::pin(async move {
                    let (doc, _) = gents_documents::read_desired_state_record_in_txn(
                        txn, collection, self.node_did(), id,
                    )
                    .await?
                    .expect("fixture document exists");
                    let name = collection.graphql_type();
                    let doc = gents_store::graphql::escape_graphql_string(&doc);
                    txn.execute_client_signed_update(
                        &format!(r#"mutation($input: {name}MutationInputArg!) {{ update_{name}(docID: "{doc}", input: $input) {{ _docID }} }}"#),
                        &json!({"input": patch}),
                        schema,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }

    pub fn core(&self) -> SelfConfigCore {
        SelfConfigCore::new(
            self.db.node.clone(),
            self.db.node_identity.did().to_owned(),
            self.agent_did.clone(),
        )
        .unwrap()
        .with_runtime_identity(Some(self.db.node_identity.clone()))
        .with_agent_signer(self.agent.managed_agent_signer())
        .with_node_binding(Some(self.node_binding.clone()))
    }

    pub async fn add_collection(&self, sdl: &str) {
        gents_store::config_client::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        )
        .add_collection(sdl)
        .await
        .unwrap();
    }

    pub fn tools(&self, categories: &[&str]) -> Vec<Box<dyn ToolDyn>> {
        let mut grants = config(categories);
        grants.agent_did = self.agent_did.clone();
        grants.preview = true;
        self.tools_with_config(&grants)
    }

    pub fn tools_with_config(&self, grants: &SelfConfigToolConfig) -> Vec<Box<dyn ToolDyn>> {
        self.tools_with_plugins(grants, test_plugins())
    }

    pub fn tools_with_plugins(
        &self,
        grants: &SelfConfigToolConfig,
        plugins: Arc<crate::plugin::executor::PluginExecutor>,
    ) -> Vec<Box<dyn ToolDyn>> {
        assert_eq!(grants.agent_did, self.agent_did);
        build_self_config_tools_with_runtime(
            Arc::default(),
            self.db.node.clone(),
            self.db.node_identity.did().to_owned(),
            Some(self.agent.clone()),
            Some(self.db.node_identity.clone()),
            grants,
            plugins,
            Arc::new(gents_tools::task_run::NoTaskRunService),
            Some(self.node_binding.clone()),
        )
    }
}

/// Every config tool access is the invoking Agent's managed signer (Lean
/// `Identity.AgentBlockSigning.configToolRoute`); the modeled cases name the
/// actor and signer each core is built with.

pub async fn config_call(
    tools: &[Box<dyn ToolDyn>],
    argv: &[&str],
) -> Result<String, gents_loop::tool::ToolError> {
    tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
        .call(json!({ "argv": argv }).to_string())
        .await
}

/// A digest names a stored pack. One the store does not hold is answered
/// with where a just-built pack is, not an OS error and a home path.

pub fn node_identity(
    label: &str,
) -> std::sync::Arc<dyn gents_store::identity::NodeIdentity> {
    let tempdir = tempfile::tempdir().expect("identity tempdir");
    std::sync::Arc::new(
        gents_store::identity::KeyIdentity::load_or_create(
            tempdir.path().join(format!("{label}.key")),
            None,
        )
        .expect("test identity"),
    )
}

pub async fn call_config_tool(
    tools: &[Box<dyn gents_loop::tool::ToolDyn>],
    argv: Vec<String>,
) -> Result<String, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    tool.call(json!({"argv": argv}).to_string())
        .await
        .map_err(|error| format!("{error:#}"))
}

pub fn take_agent_tool(
    tools: Vec<Box<dyn gents_loop::tool::ToolDyn>>,
) -> Box<dyn gents_loop::tool::ToolDyn> {
    tools
        .into_iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
}

/// Lean `SelfConfig.catalog_refused_without_grants`: choosing the default needs
/// `manage_agents` on the Node, which an Agent holding only its own grants does
/// not hold, whatever its Tools categories; the Node's owner chooses it through
/// the selection document.