use super::agent_management::{model_agent_mutate, model_agent_preview};
use super::command::{agents::agent_params, contracts::help_patch_contracts};
use super::*;

fn test_plugins() -> Arc<crate::plugin::executor::PluginExecutor> {
    Arc::new(crate::plugin::executor::PluginExecutor::default())
}

#[derive(Clone)]
struct RootReadModel {
    path: String,
    turns: Arc<std::sync::atomic::AtomicUsize>,
    provider_inputs: Arc<std::sync::Mutex<Vec<String>>>,
}

#[allow(refining_impl_trait)]
impl rig::completion::CompletionModel for RootReadModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        unreachable!("the acceptance test constructs its deterministic model directly")
    }

    async fn completion(
        &self,
        _request: rig::completion::CompletionRequest,
    ) -> Result<rig::completion::CompletionResponse<()>, rig::completion::CompletionError> {
        Err(rig::completion::CompletionError::ProviderError(
            "non-streaming completion is unused".into(),
        ))
    }

    async fn stream(
        &self,
        request: rig::completion::CompletionRequest,
    ) -> Result<rig::streaming::StreamingCompletionResponse<()>, rig::completion::CompletionError>
    {
        use std::sync::atomic::Ordering;

        crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
        if !request.tools.iter().any(|tool| tool.name == "read_file") {
            let stream: rig::streaming::StreamingResult<()> = Box::pin(futures::stream::iter([
                Ok(rig::streaming::RawStreamingChoice::Message(
                    "workspace-root-check".into(),
                )),
                Ok(rig::streaming::RawStreamingChoice::FinalResponse(())),
            ]));
            return Ok(rig::streaming::StreamingCompletionResponse::stream(stream));
        }
        self.provider_inputs
            .lock()
            .expect("provider input capture")
            .push(serde_json::to_string(&request.chat_history).expect("serialize provider input"));
        let choices = if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                rig::streaming::RawStreamingChoice::ToolCall(
                    rig::streaming::RawStreamingToolCall::new(
                        "root-read".into(),
                        "read_file".into(),
                        json!({"path": self.path}),
                    ),
                ),
                rig::streaming::RawStreamingChoice::FinalResponse(()),
            ]
        } else {
            vec![
                rig::streaming::RawStreamingChoice::Message("root check complete".into()),
                rig::streaming::RawStreamingChoice::FinalResponse(()),
            ]
        };
        let stream: rig::streaming::StreamingResult<()> =
            Box::pin(futures::stream::iter(choices.into_iter().map(Ok)));
        Ok(rig::streaming::StreamingCompletionResponse::stream(stream))
    }
}

pub(super) fn config(categories: &[&str]) -> SelfConfigToolConfig {
    SelfConfigToolConfig {
        enabled: true,
        agent_did: "beh-test".to_string(),
        categories: categories.iter().map(|c| c.to_string()).collect(),
        no_lockout: false,
        preview: false,
        enable_pack_install: false,
        process_ceiling: Default::default(),
    }
}

pub(super) struct SelfConfigFixture {
    db: crate::support::TestDb,
    user_access: crate::ConfigAccess,
    agent: Arc<dyn crate::NodeIdentity>,
    agent_did: String,
    node_binding: crate::store::config_client::ConfigurationNodeBinding,
}

impl SelfConfigFixture {
    pub(super) async fn new(name: &str) -> Self {
        let (db, _user, user_access, binding) = crate::test_support::signed_test_node(name).await;
        let agent =
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .expect("fixture Agent is derived from the held runtime");
        let response = user_access.execute("{ Node { _docID } }").await.unwrap();
        let node_binding = crate::store::config_client::ConfigurationNodeBinding {
            node_did: identity::Did::new(db.node_identity.did()).unwrap(),
            doc_id: response["data"]["Node"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned(),
            enabled: true,
            default_agent_did: Some(binding.agent_did.clone()),
        };
        Self {
            db,
            user_access,
            agent,
            agent_did: binding.agent_did,
            node_binding,
        }
    }

    pub(super) async fn grant_selected_configuration(&self) {
        self.user_access
            .transact_write("test.self_config.grant_selected", |txn| {
                let owner = self.db.node_identity.did();
                let agent = &self.agent_did;
                Box::pin(async move {
                    crate::store::config_client::grant_selected_self_configuration(
                        txn, owner, agent,
                    )
                    .await
                })
            })
            .await
            .expect("User grants selected configuration to the Agent");
    }

    pub(super) async fn install_sibling(&self, display_name: &str) -> String {
        let user = identity::Did::new(self.user_access.actor_did().unwrap()).unwrap();
        let binding = crate::store::identity::provision_agent_identity_authorized(
            &self.db.node,
            self.db.node_identity.as_ref(),
            &user,
        )
        .await
        .unwrap();
        crate::test_support::install_test_agent_binding_with_access(
            self.db.node_identity.did(),
            display_name,
            &binding,
            &self.user_access,
        )
        .await;
        binding.agent_did
    }

    pub(super) async fn grant_node_reader(&self) {
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

    async fn grant_account_fixture_selections(
        &self,
        documents: &[(crate::Collection, String, bool)],
    ) {
        self.user_access
            .transact_write("test.account_choice.grant_selections", |txn| {
                Box::pin(async move {
                    let actor = identity::Did::new(&self.agent_did)?;
                    for (collection, id, write) in documents {
                        let (doc, _) =
                            crate::store::config_client::read_desired_state_record_in_txn(
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

    async fn grant_selected_backend_writer(&self) {
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

    pub(super) async fn grant_agent_delete(&self) {
        crate::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        )
        .grant_node_permission(
            &identity::Did::new(&self.agent_did).unwrap(),
            crate::store::config_client::NodePermission::DocumentDelete,
        )
        .await
        .unwrap();
    }

    async fn grant_agent_relation_delete(&self) {
        crate::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        )
        .grant_node_permission(
            &identity::Did::new(&self.agent_did).unwrap(),
            crate::store::config_client::NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    }

    async fn grant_user_relation_delete(&self) {
        let admin = crate::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        );
        let user = identity::Did::new(self.user_access.actor_did().unwrap()).unwrap();
        admin
            .grant_node_permission(
                &user,
                crate::store::config_client::NodePermission::DacRelationDelete,
            )
            .await
            .unwrap();
    }

    pub(super) async fn grant_agent_schema(&self) {
        let admin = crate::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        );
        let actor = identity::Did::new(&self.agent_did).unwrap();
        for permission in [
            crate::store::config_client::NodePermission::CollectionGet,
            crate::store::config_client::NodePermission::CollectionPatch,
        ] {
            admin
                .grant_node_permission(&actor, permission)
                .await
                .unwrap();
        }
    }

    pub(super) fn schema_tool(&self) -> crate::tools::builtin::schema_help::SchemaTool {
        crate::tools::builtin::schema_help::SchemaTool::for_managed_agent(
            self.db.node.clone(),
            self.agent.as_ref(),
        )
        .unwrap()
    }

    pub(super) async fn grant_sibling_configuration(&self, sibling: &str) {
        self.grant_user_relation_delete().await;
        self.user_access
            .transact_write("test.self_config.grant_sibling", |txn| {
                let owner = self.db.node_identity.did();
                let actor = &self.agent_did;
                Box::pin(async move {
                    crate::store::config_client::grant_selected_self_configuration(
                        txn, owner, sibling,
                    )
                    .await?;
                    let (doc_id, _) =
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            crate::Collection::Agent,
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

    async fn grant_pack_install(&self) {
        self.grant_pack_install_to(&self.agent_did).await;
    }

    async fn grant_pack_install_to(&self, agent_did: &str) {
        use crate::store::config_client::{
            apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
            DesiredStateApplyPlan,
        };
        let tools_id = format!("{agent_did}:tools");
        self.user_access
            .transact_write("test.self_config.grant_pack_install", |txn| {
                let tools_id = &tools_id;
                let owner = self.db.node_identity.did();
                Box::pin(async move {
                    let (_, mut tools) = read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Tools,
                        owner,
                        tools_id,
                    )
                    .await?
                    .expect("selected Tools exists");
                    tools["self_config"] =
                        json!({"enable_self_config": true, "enable_pack_install": true});
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: crate::Collection::Tools,
                        add: tools.clone(),
                        update: tools,
                    }])?;
                    apply_desired_state_plan(txn, &plan).await
                })
            })
            .await
            .unwrap();
    }

    pub(super) fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub(super) fn node_did(&self) -> &str {
        self.db.node_identity.did()
    }

    pub(super) fn user_access(&self) -> &crate::ConfigAccess {
        &self.user_access
    }

    pub(super) async fn update_signed_configuration(
        &self,
        collection: crate::Collection,
        id: &str,
        patch: Value,
    ) {
        let schema = match collection {
            crate::Collection::AgentContext => gents_protocol::schemas::AGENT_CONTEXT,
            crate::Collection::InferenceProfile => gents_protocol::schemas::INFERENCE_PROFILE,
            crate::Collection::Node => gents_protocol::schemas::NODE,
            crate::Collection::Tools => gents_protocol::schemas::TOOLS,
            _ => panic!("no fixture schema for {}", collection.graphql_type()),
        };
        self.user_access
            .transact_write("test.self_config.signed_fixture_update", |txn| {
                let patch = &patch;
                Box::pin(async move {
                    let (doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                        txn, collection, self.node_did(), id,
                    )
                    .await?
                    .expect("fixture document exists");
                    let name = collection.graphql_type();
                    let doc = crate::store::graphql::escape_graphql_string(&doc);
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

    pub(super) fn core(&self) -> SelfConfigCore {
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

    pub(super) async fn add_collection(&self, sdl: &str) {
        crate::ConfigAccess::local_as(
            self.db.node.clone(),
            identity::Did::new(self.db.node_identity.did()).unwrap(),
        )
        .add_collection(sdl)
        .await
        .unwrap();
    }

    pub(super) fn tools(&self, categories: &[&str]) -> Vec<Box<dyn ToolDyn>> {
        let mut grants = config(categories);
        grants.agent_did = self.agent_did.clone();
        grants.preview = true;
        self.tools_with_config(&grants)
    }

    pub(super) fn tools_with_config(&self, grants: &SelfConfigToolConfig) -> Vec<Box<dyn ToolDyn>> {
        self.tools_with_plugins(grants, test_plugins())
    }

    fn tools_with_plugins(
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
            Some(self.node_binding.clone()),
        )
    }
}

#[tokio::test]
async fn selected_configuration_requires_and_observes_user_grant() {
    let fixture = SelfConfigFixture::new("self-config-selected-grant").await;
    let tools = fixture.tools(&["tools"]);
    let update =
        |name| json!({"argv": ["tools", "update"], "set": {"display_name": name}}).to_string();
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    assert!(tool.call(update("Denied before grant")).await.is_err());

    fixture.grant_selected_configuration().await;
    tool.call(update("Allowed"))
        .await
        .expect("User grant permits the selected Agent update");

    let admin = crate::ConfigAccess::local_as(
        fixture.db.node.clone(),
        identity::Did::new(fixture.db.node_identity.did()).unwrap(),
    );
    let user = identity::Did::new(fixture.user_access.actor_did().unwrap()).unwrap();
    admin
        .grant_node_permission(
            &user,
            crate::store::config_client::NodePermission::DacRelationDelete,
        )
        .await
        .unwrap();
    let response = fixture
        .user_access
        .execute("{ Agent { _docID } }")
        .await
        .unwrap();
    let doc_id = response["data"]["Agent"][0]["_docID"].as_str().unwrap();
    fixture
        .user_access
        .delete_actor_relationship(
            "Agent",
            doc_id,
            "config_writer",
            &identity::Did::new(&fixture.agent_did).unwrap(),
        )
        .await
        .unwrap();
    assert!(tool.call(update("Denied after revocation")).await.is_err());
}

#[test]
fn every_tool_name_is_reserved_builtin() {
    for name in SELF_CONFIG_TOOL_NAMES {
        assert!(
            crate::documents::is_reserved_builtin_tool_name(name),
            "{name} must be reserved so write_tools declarations cannot shadow it"
        );
    }
}

#[test]
fn help_contracts_conform_to_canonical_types_and_enum_vocabulary() {
    let all_contracts = [
        "agent",
        "tools",
        "profile",
        "backend",
        "mcp-service",
        "automation",
        "datastore",
        "skill",
        "agent-target",
        "execution",
    ]
    .into_iter()
    .flat_map(|resource| {
        help_patch_contracts(Some(resource))
            .as_array()
            .unwrap()
            .clone()
    })
    .collect::<Vec<_>>();
    for target in crate::store::config_client::patch::ALL_SELF_CONFIG_TARGETS {
        let contract = all_contracts
            .iter()
            .find(|contract| contract["collection"] == target.collection_name())
            .unwrap_or_else(|| panic!("missing help contract for {}", target.collection_name()));
        let described = contract["field_shapes"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let writable = target
            .writable_fields()
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            described,
            writable,
            "{} help drift",
            target.collection_name()
        );
    }

    let tools = all_contracts
        .iter()
        .find(|contract| contract["collection"] == "Tools")
        .unwrap();
    let shapes = &tools["field_shapes"];
    let assert_fields = |shape: &Value, canonical: &[&str], name: &str| {
        let described = shape
            .as_object()
            .unwrap_or_else(|| panic!("{name} help shape must be an object"))
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let canonical = canonical.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(described, canonical, "{name} nested help drift");
    };
    use crate::documents::*;
    use crate::store::config_client::canonical_struct_fields;
    assert_fields(
        &shapes["host"],
        canonical_struct_fields::<HostTools>().unwrap(),
        "host",
    );
    assert_fields(
        &shapes["host"]["files"],
        canonical_struct_fields::<FileTools>().unwrap(),
        "files",
    );
    assert_fields(
        &shapes["host"]["bash"],
        canonical_struct_fields::<BashTools>().unwrap(),
        "bash",
    );
    assert_fields(
        &shapes["host"]["cli"][0],
        canonical_struct_fields::<CliTool>().unwrap(),
        "cli[]",
    );
    assert_fields(
        &shapes["remote"],
        canonical_struct_fields::<RemoteTools>().unwrap(),
        "remote",
    );
    assert_fields(
        &shapes["agents"],
        canonical_struct_fields::<AgentTools>().unwrap(),
        "agents",
    );
    let built_in_fields = canonical_struct_fields::<BuiltInTools>()
        .unwrap()
        .iter()
        .copied()
        .filter(|field| !field.ends_with("_graph_tools"))
        .collect::<Vec<_>>();
    assert_fields(&shapes["built_ins"], &built_in_fields, "built_ins");
    assert_fields(
        &shapes["datastore"],
        canonical_struct_fields::<DatastoreTools>().unwrap(),
        "datastore",
    );
    // `eth_tool_ids` is accepted on read from v1.0.0-rc0 documents and
    // dropped; help must not offer it, so it is not part of the canonical set.
    let integration_fields = canonical_struct_fields::<IntegrationTools>()
        .unwrap()
        .iter()
        .copied()
        .filter(|field| *field != "eth_tool_ids")
        .collect::<Vec<_>>();
    assert_fields(&shapes["integrations"], &integration_fields, "integrations");
    assert_fields(
        &shapes["integrations"]["lsp"],
        canonical_struct_fields::<LspTools>().unwrap(),
        "lsp",
    );
    assert_fields(
        &shapes["self_config"],
        canonical_struct_fields::<SelfConfigTools>().unwrap(),
        "self_config",
    );

    let rendered = serde_json::to_string(&all_contracts).unwrap();
    let serialized_values = crate::inference::backends::provider::BackendProviderKind::ALL
        .into_iter()
        .map(|value| value.as_str())
        .chain(
            crate::inference::resolved::ReasoningEffort::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            crate::openai_wire::OpenAiWireApi::ALL
                .into_iter()
                .map(|value| value.as_str()),
        )
        .chain(
            crate::tools::native::CommandNetworkMode::ALL
                .into_iter()
                .map(|value| value.as_str()),
        );
    for value in serialized_values {
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
    for value in crate::tools::surface::FileToolMode::ALL
        .into_iter()
        .map(|value| serde_json::to_value(value).unwrap())
        .chain(
            crate::tools::surface::BashMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::tools::native::CommandExecutionMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::documents::RemoteToolStyle::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::documents::ConcurrencyMode::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
        .chain(
            crate::inference::compaction::CompactionStrategy::ALL
                .into_iter()
                .map(|value| serde_json::to_value(value).unwrap()),
        )
    {
        let value = value.as_str().unwrap();
        assert!(
            rendered.contains(value),
            "canonical enum value {value:?} missing from help"
        );
    }
}

#[tokio::test]
async fn build_fails_closed_without_node_did() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::Node::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::default(),
        std::sync::Arc::new(node),
        String::new(),
        None,
        &config(&["agent"]),
        test_plugins(),
    );
    assert!(
        tools.is_empty(),
        "an empty node DID must register no self-config tools"
    );
}

#[tokio::test]
async fn build_registers_gated_family() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let node = defra_node::Node::builder()
        .data_path(tempdir.path().join("data"))
        .build()
        .await
        .expect("node");
    let tools = build_self_config_tools(
        std::sync::Arc::default(),
        std::sync::Arc::new(node),
        "did:key:zSelfConfigTest".to_string(),
        None,
        &config(&["agent", "backend"]),
        test_plugins(),
    );
    assert_eq!(
        tools.len(),
        1,
        "self-configuration has one model-facing tool"
    );
    assert_eq!(tools[0].name(), CONFIG_TOOL_NAME);
    let definition = tools[0].definition(String::new()).await;
    assert!(
        definition.description.len() <= 700,
        "config description grew to {} chars",
        definition.description.len()
    );
    for core in [
        "native API",
        "copy the exact agent_did",
        "Preview writes nothing.",
        "[\"validate\"] audits saved configuration",
        "[\"help\"] lists granted resources",
    ] {
        assert!(definition.description.contains(core), "missing {core}");
    }
    assert_eq!(definition.parameters["required"], json!(["argv"]));
}

#[tokio::test]
async fn pack_install_uses_current_node_and_inference_chain() {
    let (home, plugins) = crate::test_support::home_with_fixture_pack("plugin_documents_fixture");
    let (fixture, tools) = signed_pack_tool("pack-install", plugins).await;
    let node_did = fixture.node_did();
    let coordinator = fixture.install_sibling("coordinator").await;
    let worker = fixture.install_sibling("worker").await;
    let verifier = fixture.install_sibling("verifier").await;
    fixture.grant_node_reader().await;
    let coordinator_profile = format!("{coordinator}:inference");
    let worker_profile = format!("{worker}:inference");
    let verifier_profile = format!("{verifier}:inference");
    let coordinator_slot = format!("coordinator={coordinator_profile}");
    let worker_slot = format!("worker={worker_profile}");
    let verifier_slot = format!("verifier={verifier_profile}");
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let discovery: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": ["pack", "preview", "install", "fixture/plugin_documents_fixture"]})
                    .to_string(),
            )
            .await
            .expect("incomplete preview remains readable"),
    )
    .unwrap();
    assert_eq!(discovery["committed"], false);
    assert_eq!(discovery["ready"], false);
    assert_eq!(
        discovery["missing_inference_slots"],
        json!(["coordinator", "verifier", "worker"])
    );
    assert_eq!(
        discovery["inference"][0]["profiles"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let slot_argv = [
        "--inference-slot",
        coordinator_slot.as_str(),
        "--inference-slot",
        worker_slot.as_str(),
        "--inference-slot",
        verifier_slot.as_str(),
    ];
    let mut preview_argv = vec![
        "pack",
        "preview",
        "install",
        "fixture/plugin_documents_fixture",
    ];
    preview_argv.extend(slot_argv);
    let preview: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": preview_argv}).to_string())
            .await
            .expect("complete preview succeeds"),
    )
    .unwrap();
    assert_eq!(preview["ready"], true);
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut install_argv = vec![
        "pack",
        "install",
        "fixture/plugin_documents_fixture",
        "--digest",
        digest,
    ];
    install_argv.extend(slot_argv);
    let output = tool
        .call(json!({"argv": install_argv}).to_string())
        .await
        .expect("a stored pack installs");
    let output: serde_json::Value = serde_json::from_str(&output).expect("JSON receipt");
    assert_eq!(
        output["effective"]["coordinate"],
        "fixture/plugin_documents_fixture"
    );
    assert_eq!(output["artifact_digest"], preview["artifact_digest"]);
    let record = crate::plugin::store::read_record(home.path(), "fixture", "echo_fixture")
        .expect("the pack's plugin is in the home's plugin store");
    assert_eq!(
        record.owner_pack_coordinate.as_deref(),
        Some("fixture/plugin_documents_fixture")
    );
    assert!(record.granted.is_none(), "a sealed plugin holds no grant");
    assert_eq!(
        output["inference"]["bindings"],
        json!({
            "coordinator": coordinator_profile,
            "worker": worker_profile,
            "verifier": verifier_profile,
        })
    );
    let inspected: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "get", "fixture/plugin_documents_fixture"]}).to_string())
            .await
            .expect("installed pack is inspectable"),
    )
    .unwrap();
    assert_eq!(
        inspected["installed"]["digest"],
        output["effective"]["digest"]
    );

    let (_staged_root, staged) =
        crate::test_support::fixture_pack_copy("plugin_documents_fixture", &json!({}));
    let manifest_path = staged.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["version"] = "1.1.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (bytes, _) = crate::pack::archive::pack_dir(&staged).unwrap();
    let staged_artifact = crate::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap();
    let mut staged_preview = vec![
        "pack",
        "preview",
        "update",
        staged_artifact.header.digest.as_str(),
    ];
    staged_preview.extend(slot_argv);
    let preview: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": staged_preview}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    let listed: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "list"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    let entry = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "fixture/plugin_documents_fixture")
        .unwrap();
    assert_eq!(entry["version"], output["effective"]["version"]);
    assert_eq!(entry["installed_digest"], output["effective"]["digest"]);
    assert_eq!(entry["available_version"], "1.1.0");
    assert_eq!(entry["other_stored_versions"][0]["version"], "1.1.0");

    let bad_digest = format!("sha256:{}", "0".repeat(64));
    let rejected = tool
        .call(
            json!({"argv": [
                "pack", "update", "fixture/plugin_documents_fixture@1.0.0", "--digest", bad_digest,
                "--inference-slot", coordinator_slot.as_str(),
                "--inference-slot", worker_slot.as_str(),
                "--inference-slot", verifier_slot.as_str()
            ]})
            .to_string(),
        )
        .await
        .expect_err("a digest not returned by preview is rejected");
    assert!(rejected.to_string().contains("preview again"));

    let rows = fixture
        .user_access
        .execute(
            r#"{ Agent(filter: {display_name: {_eq: "Fixture worker"}}) { _docID agent_did } }"#,
        )
        .await
        .unwrap();
    let pack_worker = rows["data"]["Agent"][0]["_docID"].as_str().unwrap();
    let doc = crate::store::graphql::escape_graphql_string(pack_worker);
    fixture.user_access.transact_write("test.pack.tag_worker", |txn| {
        let doc = &doc;
        Box::pin(async move {
            txn.execute_client_signed_update(
                &format!(r#"mutation($input: AgentMutationInputArg!) {{ update_Agent(docID: "{doc}", input: $input) {{ _docID }} }}"#),
                &json!({"input": {"tags": ["gents:pack:plugin_documents_fixture", "user:favorite"]}}),
                gents_protocol::schemas::AGENT,
            ).await
        })
    }).await.unwrap();
    let update_preview: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": [
                    "pack", "preview", "update", "fixture/plugin_documents_fixture@1.0.0",
                    "--inference-slot", coordinator_slot.as_str(),
                    "--inference-slot", worker_slot.as_str(),
                    "--inference-slot", verifier_slot.as_str()
                ]})
                .to_string(),
            )
            .await
            .expect("installed pack update previews"),
    )
    .unwrap();
    assert_eq!(update_preview["ready"], true);
    let update_digest = update_preview["artifact_digest"].as_str().unwrap();
    let updated: Value = serde_json::from_str(
        &tool
            .call(
                json!({"argv": [
                    "pack", "update", "fixture/plugin_documents_fixture@1.0.0", "--digest", update_digest,
                    "--inference-slot", coordinator_slot.as_str(),
                    "--inference-slot", worker_slot.as_str(),
                    "--inference-slot", verifier_slot.as_str()
                ]})
                .to_string(),
            )
            .await
            .expect("same distribution update is idempotent"),
    )
    .unwrap();
    assert_eq!(
        updated["effective"]["digest"],
        output["effective"]["digest"]
    );
    let tags = fixture
        .user_access
        .execute(&format!(
            r#"{{ Agent(docID: "{}") {{ tags }} }}"#,
            crate::store::graphql::escape_graphql_string(pack_worker),
        ))
        .await
        .unwrap();
    assert_eq!(
        tags["data"]["Agent"][0]["tags"],
        json!(["gents:pack:plugin_documents_fixture", "user:favorite"])
    );
    let store = crate::pack::store::PackStore::new(home.path());
    let installed_path = store
        .path(output["effective"]["digest"].as_str().unwrap())
        .unwrap();
    let installed_bytes = std::fs::read(&installed_path).unwrap();
    std::fs::remove_file(&installed_path).unwrap();
    std::fs::remove_dir_all(home.path().join("packs").join("unpacked")).unwrap();
    let listed: Value = serde_json::from_str(
        &tool
            .call(json!({"argv": ["pack", "list"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    let entry = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "fixture/plugin_documents_fixture")
        .unwrap();
    assert_eq!(entry["installed_digest"], output["effective"]["digest"]);
    assert_eq!(entry["installable"], false);
    assert!(entry["error"]
        .as_str()
        .unwrap()
        .contains("could not be opened"));
    std::fs::write(installed_path, installed_bytes).unwrap();
    let removed = tool
        .call(json!({"argv": ["pack", "remove", "fixture/plugin_documents_fixture"]}).to_string())
        .await
        .expect("an installed pack removes");
    let removed: Value = serde_json::from_str(&removed).unwrap();
    assert_eq!(removed["pack"], "fixture/plugin_documents_fixture");

    let response = fixture
        .user_access
        .execute(&format!(
            r#"{{ PackInstallation(filter: {{node_did: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            crate::store::graphql::escape_graphql_string(node_did),
        ))
        .await
        .unwrap();
    assert!(response["data"]["PackInstallation"]
        .as_array()
        .unwrap()
        .is_empty());

    let again = tool
        .call(json!({"argv": ["pack", "remove", "fixture/plugin_documents_fixture"]}).to_string())
        .await
        .expect_err("a second remove finds no record");
    assert!(again.to_string().contains("is not installed"), "{again:#}");
}

async fn signed_pack_tool(
    label: &str,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let mut tool_config = config(&[]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    let tools = fixture.tools_with_plugins(&tool_config, plugins);
    (fixture, tools)
}

async fn config_call(
    tools: &[Box<dyn ToolDyn>],
    argv: &[&str],
) -> Result<String, crate::inference::llm::tool::ToolError> {
    tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
        .call(json!({ "argv": argv }).to_string())
        .await
}

#[tokio::test]
async fn pack_list_pages_the_packs_the_home_store_holds() {
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (_fixture, tools) = signed_pack_tool("pack-list", plugins).await;
    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["source"], "store_and_installations");
    assert_eq!(page["page"]["total"], 1);
    assert_eq!(page["page"]["truncated"], false);
    assert_eq!(page["items"][0]["name"], "fixture/documents_fixture");
    assert_eq!(page["items"][0]["version"], Value::Null);
    assert_eq!(page["items"][0]["available_version"], "1.0.0");
    assert_eq!(
        page["items"][0]["other_stored_versions"][0]["version"],
        "1.0.0"
    );
    assert_eq!(page["items"][0]["installable"], true);
    assert_eq!(page["items"][0]["installed"], Value::Null);

    let after: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "list", "--cursor", "fixture/documents_fixture"],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(after["items"].as_array().unwrap().is_empty());

    let error = config_call(&tools, &["pack", "list", "--limit", "51"])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("between 1 and 50"), "{error}");

    let (_fixture, homeless) = signed_pack_tool("pack-list-homeless", test_plugins()).await;
    let page: Value =
        serde_json::from_str(&config_call(&homeless, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["page"]["total"], 0);
    assert!(page["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn pack_list_reports_a_damaged_archive_as_an_error_row() {
    let (home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (_fixture, tools) = signed_pack_tool("pack-list-damaged", plugins).await;
    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    let digest = page["items"][0]["artifact_digest"].as_str().unwrap();
    let path = crate::pack::store::PackStore::new(home.path())
        .path(digest)
        .unwrap();
    std::fs::write(path, b"not a pack archive").unwrap();
    std::fs::remove_dir_all(home.path().join("packs").join("unpacked")).unwrap();

    let page: Value =
        serde_json::from_str(&config_call(&tools, &["pack", "list"]).await.unwrap()).unwrap();
    assert_eq!(page["page"]["returned"], 1);
    assert_eq!(page["items"][0]["name"], "fixture/documents_fixture");
    assert_eq!(page["items"][0]["installable"], false);
    assert!(page["items"][0]["error"]
        .as_str()
        .unwrap()
        .contains("could not be opened"));
}

static PACK_REGISTRY_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn pack_update_without_a_version_asks_the_registry_and_fails_loudly_offline() {
    let _registry_lock = PACK_REGISTRY_ENV.lock().await;
    let _registry = crate::test_support::EnvVarGuard::set("GENTS_REGISTRY", "http://127.0.0.1:9");
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (_fixture, tools) = signed_pack_tool("pack-update-offline", plugins).await;
    let error = config_call(
        &tools,
        &["pack", "preview", "update", "fixture/documents_fixture"],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("the newest version of fixture/documents_fixture could not be looked up"),
        "{error}"
    );
    let error = config_call(
        &tools,
        &[
            "pack",
            "preview",
            "update",
            "fixture/documents_fixture@1.0.0",
        ],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("is not installed"), "{error}");
}

#[tokio::test]
async fn pack_install_refuses_a_pack_shipped_job_block() {
    let (_guard, dir) = crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let config_path = dir.join("pack_config.json");
    let mut pack_config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    pack_config["tools"][0]["integrations"] =
        json!({"plugins": [{"plugin": "acme/runner", "job": {"agent_targets": ["wf-run"]}}]});
    pack_config["agent_targets"] = json!([{"target_id": "wf-run", "target_node_did": "did:key:foreign", "agent_did": "fixture-worker", "name": "wf-run"}]);
    std::fs::write(&config_path, serde_json::to_vec(&pack_config).unwrap()).unwrap();
    let (_home, plugins) = crate::test_support::home_with_pack_dir(&dir);
    let (fixture, tools) = signed_pack_tool("pack-job-block", plugins).await;
    let slot = format!("worker={}:inference", fixture.agent_did());
    let error = format!(
        "{:#}",
        config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                "fixture/documents_fixture",
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap_err()
    );
    assert!(error.contains("operator install"), "{error}");
}

#[tokio::test]
async fn pack_install_refuses_tools_naming_an_unresolved_plugin() {
    let (_guard, dir) = crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let config_path = dir.join("pack_config.json");
    let mut pack_config: Value =
        serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    pack_config["tools"][0]["integrations"] = json!({"plugins": [{"plugin": "acme/missing"}]});
    std::fs::write(&config_path, serde_json::to_vec(&pack_config).unwrap()).unwrap();
    let (_home, plugins) = crate::test_support::home_with_pack_dir(&dir);
    let (fixture, tools) = signed_pack_tool("pack-unresolved-plugin", plugins).await;
    let slot = format!("worker={}:inference", fixture.agent_did());
    let error = format!(
        "{:#}",
        config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                "fixture/documents_fixture",
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap_err()
    );
    assert!(
        error.contains("acme/missing") && error.contains("fixture-worker-tools"),
        "{error}"
    );
    let written = fixture
        .user_access
        .execute(r#"{ Tools(filter: {tools_id: {_eq: "fixture-worker-tools"}}) { tools_id } }"#)
        .await
        .unwrap();
    assert!(written["data"]["Tools"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn config_tools_cannot_self_grant_pack_install() {
    let fixture = SelfConfigFixture::new("pack-self-grant").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["tools"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let error = tool
        .call(
            serde_json::json!({
                "argv": ["tools", "edit", "--set", format!("self_config={}", json!({
                        "enable_self_config": true,
                        "enable_pack_install": true
                    }))]
            })
            .to_string(),
        )
        .await
        .expect_err("pack install cannot be self-granted");
    assert!(
        error.to_string().contains("cannot be self-granted"),
        "{error:#}"
    );
}

#[tokio::test]
async fn config_tools_unrelated_edit_on_granted_tools_is_accepted() {
    let fixture = SelfConfigFixture::new("granted-tools-edit").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let tools = fixture.tools(&["tools"]);

    let edited = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--set".into(),
            r#"agents={"enabled":true}"#.into(),
        ],
    )
    .await
    .expect("an edit that raises no grant is accepted without holding it");
    assert_eq!(
        serde_json::from_str::<Value>(&edited).unwrap()["committed"],
        true
    );
    call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--set".into(),
            format!(
                "self_config={}",
                json!({"enable_self_config": true, "enable_pack_install": true,
                       "self_config_preview": true})
            ),
        ],
    )
    .await
    .expect("keeping a stored grant while rewriting its group is not a raise");
}

#[tokio::test]
async fn config_tools_holder_may_grant_pack_install_to_a_sibling() {
    let fixture = SelfConfigFixture::new("pack-holder-sibling").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let sibling = fixture.install_sibling("sibling").await;
    fixture.grant_node_reader().await;
    fixture.grant_sibling_configuration(&sibling).await;
    let mut tool_config = config(&["node", "tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    let tools = fixture.tools_with_config(&tool_config);
    let applied = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            sibling.clone(),
            "--set".into(),
            r#"self_config={"enable_self_config":true,"enable_pack_install":true}"#.into(),
        ],
    )
    .await
    .expect("a holder may grant pack installation to a sibling");
    assert_eq!(
        serde_json::from_str::<Value>(&applied).unwrap()["committed"],
        true
    );
}

fn argv_of(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn config_error_message(error: crate::inference::llm::tool::ToolError) -> String {
    let crate::inference::llm::tool::ToolError::ToolCallError(error) = error else {
        panic!("missing typed config error: {error}");
    };
    let envelope: Value =
        serde_json::from_str(&error.to_string()).expect("a config refusal is a JSON envelope");
    envelope["error"]
        .as_str()
        .expect("the envelope carries an error message")
        .to_owned()
}

async fn config_call_message(
    tools: &[Box<dyn crate::inference::llm::tool::ToolDyn>],
    argv: Vec<String>,
) -> Result<String, String> {
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    tool.call(json!({ "argv": argv }).to_string())
        .await
        .map_err(config_error_message)
}

struct ReselectionFixture {
    _fixture: SelfConfigFixture,
    sibling: String,
    granted: String,
    granted2: String,
    tools: Vec<Box<dyn crate::inference::llm::tool::ToolDyn>>,
}

impl std::ops::Deref for ReselectionFixture {
    type Target = [Box<dyn crate::inference::llm::tool::ToolDyn>];
    fn deref(&self) -> &Self::Target {
        &self.tools
    }
}

async fn reselection_tools(label: &str, hold_pack_install: bool) -> ReselectionFixture {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let sibling = fixture.install_sibling("sibling").await;
    let granted = fixture.install_sibling("granted").await;
    let granted2 = fixture.install_sibling("granted-2").await;
    for target in [&sibling, &granted, &granted2] {
        fixture.grant_sibling_configuration(target).await;
    }
    fixture.grant_pack_install_to(&granted).await;
    fixture.grant_pack_install_to(&granted2).await;
    if hold_pack_install {
        fixture.grant_pack_install().await;
    }
    let mut tool_config = config(&["node", "agent", "tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = hold_pack_install;
    let tools = fixture.tools_with_config(&tool_config);
    ReselectionFixture {
        _fixture: fixture,
        sibling,
        granted,
        granted2,
        tools,
    }
}

#[tokio::test]
async fn context_reselect_cannot_acquire_grants() {
    let tools = reselection_tools("context-reselect", false).await;
    let granted_tools = format!("{}:tools", tools.granted);
    let selection = format!("tools_id=\"{granted_tools}\"");
    for target in [None, Some(tools.sibling.as_str())] {
        let mut argv = argv_of(&["agent", "context", "edit"]);
        if let Some(target) = target {
            argv.extend(argv_of(&["--agent", target]));
        }
        argv.extend(argv_of(&["--set", &selection]));
        let refused = config_call_message(&tools, argv)
            .await
            .expect_err("re-pointing a Context at granted Tools must not acquire the grant");
        assert!(refused.contains(&format!("tools_id {granted_tools:?} selects Tools carrying an operator grant this agent does not hold")), "{target:?}: {refused}");
        assert!(
            refused.contains("cannot be self-granted"),
            "{target:?}: {refused}"
        );
    }
    let missing = call_config_tool(
        &tools,
        argv_of(&["agent", "context", "edit", "--set", r#"tools_id="missing""#]),
    )
    .await
    .expect_err("a missing Tools reference is still refused");
    assert!(
        !missing.contains("operator grant"),
        "the reference validator refuses a missing document: {missing}"
    );
    let holder = reselection_tools("context-reselect-holder", true).await;
    let selection = format!("tools_id=\"{}:tools\"", holder.granted);
    call_config_tool(
        &holder,
        argv_of(&["agent", "context", "edit", "--set", &selection]),
    )
    .await
    .expect("an agent holding the grant may select Tools that carry it");
}

#[tokio::test]
async fn agent_reselect_cannot_acquire_grants() {
    let tools = reselection_tools("agent-reselect", false).await;
    let granted_context = format!("{}:context", tools.granted);
    let selection = format!("context_id=\"{granted_context}\"");
    for target in [tools._fixture.agent_did(), tools.sibling.as_str()] {
        let refused = config_call_message(
            &tools,
            argv_of(&["agent", "edit", target, "--set", &selection]),
        )
        .await
        .expect_err("re-pointing an Agent at a granted chain must not acquire the grant");
        assert!(refused.contains(&format!("context_id {granted_context:?} selects Tools carrying an operator grant this agent does not hold")), "{target}: {refused}");
    }
    let holder = reselection_tools("agent-reselect-holder", true).await;
    let selection = format!("context_id=\"{}:context\"", holder.granted);
    call_config_tool(
        &holder,
        argv_of(&["agent", "edit", &holder.sibling, "--set", &selection]),
    )
    .await
    .expect("an agent holding the grant may select a chain that carries it");
}

#[tokio::test]
async fn context_reselect_between_granted_tools_is_accepted() {
    let tools = reselection_tools("context-between-granted", false).await;
    let selection = format!("tools_id=\"{}:tools\"", tools.granted2);
    call_config_tool(
        &tools,
        argv_of(&[
            "agent",
            "context",
            "edit",
            "--agent",
            &tools.granted,
            "--set",
            &selection,
        ]),
    )
    .await
    .expect("re-pointing between granted Tools raises nothing");
}

#[tokio::test]
async fn context_create_selecting_granted_tools_requires_the_grant() {
    let tools = reselection_tools("context-create-granted", false).await;
    let selection = format!("tools_id=\"{}:tools\"", tools.granted);
    let create = argv_of(&["context", "create", "fresh:context", "--set", &selection]);
    let refused = config_call_message(&tools, create)
        .await
        .expect_err("a new Context must not select Tools whose grant the agent does not hold");
    assert!(
        refused.contains("selects Tools carrying an operator grant this agent does not hold"),
        "{refused}"
    );
    let holder = reselection_tools("context-create-holder", true).await;
    let selection = format!("tools_id=\"{}:tools\"", holder.granted);
    call_config_tool(
        &holder,
        argv_of(&["context", "create", "fresh:context", "--set", &selection]),
    )
    .await
    .expect("an agent holding the grant may create a Context that selects it");
}

pub(super) fn node_identity(label: &str) -> std::sync::Arc<dyn crate::NodeIdentity> {
    let tempdir = tempfile::tempdir().expect("identity tempdir");
    std::sync::Arc::new(
        crate::KeyIdentity::load_or_create(tempdir.path().join(format!("{label}.key")), None)
            .expect("test identity"),
    )
}

#[tokio::test]
async fn automation_rejects_invalid_template_before_publication_and_can_recover() {
    let fixture = SelfConfigFixture::new("template-config").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["automation"]);
    for verb in ["preview", "edit"] {
        let error = call_config_tool(
            &tools,
            vec![
                "automation".into(),
                verb.into(),
                "task".into(),
                "template-check".into(),
                "--set".into(),
                "prompt_template=\"{{.message}}\"".into(),
            ],
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("MiniJinja"), "{error:#}");
        let rows = fixture
            .user_access
            .execute("{ Task { task_id } }")
            .await
            .unwrap();
        assert!(rows["data"]["Task"].as_array().unwrap().is_empty());
    }
    call_config_tool(
        &tools,
        vec![
            "automation".into(),
            "edit".into(),
            "task".into(),
            "template-check".into(),
            "--set".into(),
            "prompt_template=\"Process {{ doc.message }}\"".into(),
        ],
    )
    .await
    .unwrap();
    let rows = fixture
        .user_access
        .execute("{ Task { task_id prompt_template } }")
        .await
        .unwrap();
    let data = &rows["data"];
    assert_eq!(data["Task"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["Task"][0]["prompt_template"],
        "Process {{ doc.message }}"
    );
}

#[tokio::test]
async fn automation_rejects_a_count_field_the_runtime_cannot_read_and_can_recover() {
    let fixture = SelfConfigFixture::new("event-source-config").await;
    fixture.grant_selected_configuration().await;
    fixture
        .add_collection("type SelfConfigProbe { batch: String flag: Boolean total: Int }")
        .await;
    let tools = fixture.tools(&["automation"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered");
    let set = |count_field: &str| {
        json!({
            "argv": ["automation", "edit", "event-source"],
            "target_id": "probe",
            "set": {
                "source_collection": "SelfConfigProbe",
                "correlation_field": "batch",
                "group": {"expected_count": {"source_field": count_field}}
            }
        })
        .to_string()
    };
    for verb in ["preview", "edit"] {
        let argv = json!({
            "argv": ["automation", verb, "event-source"],
            "target_id": "probe",
            "set": {
                "source_collection": "SelfConfigProbe",
                "correlation_field": "batch",
                "group": {"expected_count": {"source_field": "flag"}}
            }
        })
        .to_string();
        let error = tool.call(argv).await.unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("cannot carry the count"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("Boolean"), "{diagnostic}");
        let rows = fixture
            .user_access
            .execute("{ EventSource { event_source_id } }")
            .await
            .unwrap();
        assert!(
            rows["data"]["EventSource"].as_array().unwrap().is_empty(),
            "a refused event source publishes no row"
        );
    }
    tool.call(set("total")).await.unwrap();
    let rows = fixture
        .user_access
        .execute("{ EventSource { event_source_id group } }")
        .await
        .unwrap();
    let rows = rows["data"]["EventSource"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["group"]["expected_count"]["source_field"], "total");
}

#[tokio::test]
async fn effective_config_reports_inherited_workspace_network_as_disabled() {
    let fixture = SelfConfigFixture::new("workspace-network-report").await;
    fixture.grant_selected_configuration().await;
    let root = tempfile::tempdir().unwrap();
    let core =
        fixture
            .core()
            .with_process_ceiling(crate::tools::surface::SelfConfigProcessCeiling {
                file_mode: crate::tools::surface::FileToolMode::ReadWrite,
                bash_mode: crate::tools::surface::BashMode::Unrestricted,
                root: Some(root.path().into()),
            });
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(),
                "bash": {"mode": "Unrestricted", "execution_mode": "workspace_write"}
            })),
        )],
    ))
    .await
    .unwrap();

    let report = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    assert_eq!(
        report["tool_grants"]["configured"]["network_mode"],
        "inherit"
    );
    assert_eq!(
        report["runtime_effective"]["effective"]["network_mode"],
        "disabled"
    );
}

#[tokio::test]
async fn skill_import_previews_without_writes_and_requires_file_authority() {
    let fixture = SelfConfigFixture::new("skill-import").await;
    fixture.grant_selected_configuration().await;
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("SKILL.md");
    std::fs::write(
        &file,
        "---\nname: Review\ndescription: Review code\n---\nCheck the diff carefully.",
    )
    .unwrap();
    let mut grants = config(&["tools", "agent"]);
    grants.agent_did = fixture.agent_did().to_owned();
    grants.preview = true;
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let denied_tools = fixture.tools_with_config(&grants);
    let denied = call_config_tool(
        &denied_tools,
        command(&["skill", "import", "review", file.to_str().unwrap()]),
    )
    .await
    .unwrap_err();
    assert!(denied.contains("file read permission"), "{denied}");
    grants.process_ceiling = crate::tools::surface::SelfConfigProcessCeiling {
        file_mode: crate::tools::surface::FileToolMode::ReadOnly,
        bash_mode: crate::tools::surface::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(), "files": {"mode": "ReadOnly"}
            })),
        )],
    ))
    .await
    .unwrap();
    let tools = fixture.tools_with_config(&grants);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("SKILL.md"),
        "Outside the effective root.",
    )
    .unwrap();
    let rejected = call_config_tool(
        &tools,
        command(&[
            "skill",
            "import",
            "outside",
            outside.path().to_str().unwrap(),
        ]),
    )
    .await
    .unwrap_err();
    assert!(
        rejected.contains("outside the allowed tool root"),
        "{rejected}"
    );
    assert!(
        call_config_tool(&tools, command(&["skill", "get", "outside"]))
            .await
            .is_err()
    );
    let preview = call_config_tool(
        &tools,
        command(&[
            "skill",
            "preview",
            "import",
            "review",
            file.to_str().unwrap(),
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&preview).unwrap()["committed"],
        false
    );
    assert!(
        call_config_tool(&tools, command(&["skill", "get", "review"]))
            .await
            .is_err()
    );
    call_config_tool(
        &tools,
        command(&["skill", "import", "review", root.path().to_str().unwrap()]),
    )
    .await
    .unwrap();
    assert!(call_config_tool(
        &tools,
        command(&["skill", "import", "review", file.to_str().unwrap()])
    )
    .await
    .is_err());
    let read = call_config_tool(&tools, command(&["skill", "get", "review"]))
        .await
        .unwrap();
    assert!(read.contains("Check the diff carefully."));
    assert!(read.contains("source_directory"));
    assert!(read.contains(root.path().canonicalize().unwrap().to_str().unwrap()));
    call_config_tool(
        &tools,
        command(&[
            "agent",
            "context",
            "edit",
            "--set",
            "skill_ids=[\"review\"]",
        ]),
    )
    .await
    .unwrap();
    let effective = core
        .with_process_ceiling(grants.process_ceiling)
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    assert_eq!(effective["context"]["skill_ids"], json!(["review"]));
    assert_eq!(
        effective["runtime_effective"]["effective"]["file_mode"],
        "ReadOnly"
    );
    assert_eq!(
        effective["runtime_effective"]["effective"]["bash_mode"],
        "Off"
    );
}

#[tokio::test]
async fn configuration_discovery_is_read_only_root_bounded_and_sanitized() {
    let fixture = SelfConfigFixture::new("configuration-discovery").await;
    fixture.grant_selected_configuration().await;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("synthetic-codex");
    std::fs::create_dir_all(&source).unwrap();
    let marker = root.path().join("SHOULD_NEVER_RUN");
    let source_config =
        include_str!("../../../../tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &marker.to_string_lossy());
    std::fs::write(source.join("config.toml"), source_config).unwrap();

    let mut grants = config(&["tools"]);
    grants.agent_did = fixture.agent_did().to_owned();
    let denied_tools = fixture.tools_with_config(&grants);
    let command = |args: &[&str]| args.iter().map(|value| (*value).to_owned()).collect();
    let help = call_config_tool(&denied_tools, command(&["help", "discovery"]))
        .await
        .unwrap();
    assert!(help.starts_with("discovery: "), "{help}");
    let legacy = call_config_tool(&denied_tools, command(&["discover", "scan"]))
        .await
        .unwrap_err();
    assert!(
        legacy.contains("unknown config resource or command"),
        "{legacy}"
    );
    let denied = call_config_tool(
        &denied_tools,
        command(&[
            "discovery",
            "scan",
            "--source",
            "codex-user",
            "codex",
            "user",
            source.to_str().unwrap(),
        ]),
    )
    .await
    .unwrap_err();
    assert!(denied.contains("file read permission"), "{denied}");

    grants.process_ceiling = crate::tools::surface::SelfConfigProcessCeiling {
        file_mode: crate::tools::surface::FileToolMode::ReadOnly,
        bash_mode: crate::tools::surface::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(), "files": {"mode": "ReadOnly"}
            })),
        )],
    ))
    .await
    .unwrap();
    let before = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();
    let tools = fixture.tools_with_config(&grants);
    let outside = tempfile::tempdir().unwrap();
    let outside_error = call_config_tool(
        &tools,
        command(&[
            "discovery",
            "scan",
            "--source",
            "outside",
            "codex",
            "user",
            outside.path().to_str().unwrap(),
        ]),
    )
    .await
    .unwrap_err();
    assert!(
        outside_error.contains("outside the allowed tool root"),
        "{outside_error}"
    );
    let output = call_config_tool(
        &tools,
        command(&[
            "discovery",
            "scan",
            "--source",
            "codex-user",
            "codex",
            "user",
            source.to_str().unwrap(),
        ]),
    )
    .await
    .unwrap();
    let after = core
        .read_effective_config(&BTreeSet::new(), false, false)
        .await
        .unwrap();

    assert_eq!(before, after, "discovery must not mutate configuration");
    assert!(!marker.exists(), "discovery must not execute MCP commands");
    assert!(!output.contains("FAKE_DISCOVERY_SECRET_123"));
    assert!(!output.contains("SHOULD_NEVER_RUN"));
    let inventory: crate::tools::builtin::self_config::configuration_discovery::ConfigurationDiscoveryInventory =
        serde_json::from_str(&output).unwrap();
    assert_eq!(inventory.schema_version, 1);
    assert!(inventory.items.iter().any(|item| {
        item.category == crate::tools::builtin::self_config::configuration_discovery::DiscoveryCategory::RemoteTool
            && item.display_label == "shared"
    }));
}

#[tokio::test]
async fn setup_discovery_clarification_apply_and_verification_preserve_disabled_settings() {
    use crate::tools::builtin::self_config::configuration_discovery::{
        ConfigurationDiscoveryInventory, DiscoveryCategory, DiscoveryItemState,
        DiscoverySourceOutcome, MappingSupportLevel,
    };

    let fixture = SelfConfigFixture::new("discovery-apply").await;
    fixture.grant_selected_configuration().await;
    let node = fixture.db.node.clone();
    let owner = fixture.node_did().to_owned();
    let identity = fixture.agent.clone();
    let agent_did = fixture.agent_did().to_owned();
    let runtime_access =
        crate::ConfigAccess::local_as(node.clone(), identity::Did::new(&owner).unwrap());
    let agent_access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(&agent_did).unwrap(),
        identity.as_ref(),
    )
    .unwrap();

    let root = tempfile::tempdir().unwrap();
    let user_root = root.path().join("synthetic-user/.codex");
    let project_root = root.path().join("synthetic-project");
    std::fs::create_dir_all(&user_root).unwrap();
    std::fs::create_dir_all(project_root.join(".codex")).unwrap();
    let marker = root.path().join("SHOULD_NEVER_RUN");
    std::fs::write(
        user_root.join("config.toml"),
        include_str!("../../../../tests/fixtures/configuration_discovery/codex-user/config.toml")
            .replace("SHOULD_NEVER_RUN", &marker.to_string_lossy()),
    )
    .unwrap();
    std::fs::write(
        user_root.join("AGENTS.md"),
        include_str!("../../../../tests/fixtures/configuration_discovery/codex-user/AGENTS.md"),
    )
    .unwrap();
    std::fs::write(
        project_root.join(".codex/config.toml"),
        include_str!(
            "../../../../tests/fixtures/configuration_discovery/project/.codex/config.toml"
        ),
    )
    .unwrap();
    std::fs::write(
        project_root.join("AGENTS.md"),
        include_str!("../../../../tests/fixtures/configuration_discovery/project/AGENTS.md"),
    )
    .unwrap();

    let mut grants = config(&["tools", "agent", "backend"]);
    grants.preview = true;
    grants.agent_did = agent_did.clone();
    grants.process_ceiling = crate::tools::surface::SelfConfigProcessCeiling {
        file_mode: crate::tools::surface::FileToolMode::ReadOnly,
        bash_mode: crate::tools::surface::BashMode::Off,
        root: Some(root.path().into()),
    };
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "host".into(),
            Some(json!({
                "root": root.path().to_str().unwrap(),
                "files": {"mode": "ReadOnly"}
            })),
        )],
    ))
    .await
    .unwrap();
    let tools = fixture.tools_with_config(&grants);
    let scan = vec![
        "discovery".into(),
        "scan".into(),
        "--source".into(),
        "fixture-user-codex".into(),
        "codex".into(),
        "user".into(),
        user_root.to_string_lossy().into_owned(),
        "--source".into(),
        "fixture-project-codex".into(),
        "codex".into(),
        "project".into(),
        project_root.to_string_lossy().into_owned(),
    ];

    let discovered: ConfigurationDiscoveryInventory =
        serde_json::from_str(&call_config_tool(&tools, scan.clone()).await.unwrap()).unwrap();
    assert!(discovered
        .sources
        .iter()
        .all(|source| source.outcome == DiscoverySourceOutcome::Complete));
    assert!(!discovered.conflicts.is_empty());
    assert!(
        !marker.exists(),
        "discovery must not execute configured commands"
    );
    let serialized = serde_json::to_string(&discovered).unwrap();
    assert!(!serialized.contains("FAKE_DISCOVERY_SECRET_123"));
    assert!(!serialized.contains("SHOULD_NEVER_RUN"));

    let approved = discovered
        .items
        .iter()
        .find(|item| {
            item.source_id == "fixture-project-codex"
                && item.category == DiscoveryCategory::Instruction
        })
        .expect("project instruction candidate");
    let disabled = discovered
        .items
        .iter()
        .find(|item| {
            item.source_id == "fixture-project-codex"
                && item.category == DiscoveryCategory::RemoteTool
        })
        .expect("disabled project remote tool");
    assert_eq!(disabled.state, DiscoveryItemState::Disabled);
    assert_eq!(disabled.mapping.level, MappingSupportLevel::Partial);
    assert!(!disabled.mapping.reasons.is_empty());
    assert!(
        discovered
            .items
            .iter()
            .filter(|item| { item.category == DiscoveryCategory::ModelPreference })
            .count()
            >= 2
    );

    let command = |args: &[&str]| args.iter().map(|value| (*value).to_owned()).collect();
    let context_before = call_config_tool(
        &tools,
        command(&["agent", "context", "get", "--agent", &agent_did]),
    )
    .await
    .unwrap();
    async fn runtime_backend(access: &crate::ConfigAccess, owner: &str, id: &str) -> Value {
        access
            .transact_readonly("test.self_config.backend", |txn| {
                Box::pin(async move {
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::InferenceBackend,
                        owner,
                        id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("runtime backend")
            .1
    }
    let backend_id = format!("{agent_did}:backend");
    let backend_before = runtime_backend(&runtime_access, &owner, &backend_id).await;
    assert!(!backend_before
        .to_string()
        .contains("FAKE_DISCOVERY_SECRET_123"));
    let services_before =
        crate::tools::mcp::registry::configured_mcp_services_on(&agent_access, &owner)
            .await
            .unwrap();

    let approved_prompt = format!(
        "Review repository changes and report evidence. Source: {}.",
        approved.relative_source_file
    );
    let prompt_patch = format!(
        "system_prompt={}",
        serde_json::to_string(&approved_prompt).unwrap()
    );
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "context".into(),
                "preview".into(),
                "--agent".into(),
                agent_did.clone(),
                "--set".into(),
                prompt_patch.clone(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(
        call_config_tool(
            &tools,
            command(&["agent", "context", "get", "--agent", &agent_did]),
        )
        .await
        .unwrap(),
        context_before,
        "preview must not apply the proposal"
    );

    call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
            agent_did.clone(),
            "--set".into(),
            prompt_patch,
        ],
    )
    .await
    .unwrap();

    let verified: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            command(&["agent", "context", "get", "--agent", &agent_did]),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(verified["document"]["system_prompt"], approved_prompt);
    assert_eq!(
        runtime_backend(&runtime_access, &owner, &backend_id).await,
        backend_before,
        "discovery-backed setup must not modify operator-owned inference credentials"
    );
    assert_eq!(
        crate::tools::mcp::registry::configured_mcp_services_on(&agent_access, &owner)
            .await
            .unwrap()
            .len(),
        services_before.len(),
        "a disabled discovered remote tool must not be configured or enabled"
    );

    let verified_discovery: ConfigurationDiscoveryInventory =
        serde_json::from_str(&call_config_tool(&tools, scan).await.unwrap()).unwrap();
    let verified_disabled = verified_discovery
        .items
        .iter()
        .find(|item| item.item_id == disabled.item_id)
        .expect("disabled item survives verification scan");
    assert_eq!(verified_disabled.state, DiscoveryItemState::Disabled);
    assert_eq!(verified_disabled.mapping, disabled.mapping);
}

#[tokio::test]
async fn datastore_preview_create_and_sparse_edit_use_owned_patch_path() {
    let fixture = SelfConfigFixture::new("datastore-config").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["tools"]);
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect();
    let expected_help = call_config_tool(&tools, command(&["help", "datastore"]))
        .await
        .unwrap();
    assert_eq!(
        call_config_tool(&tools, command(&["datastore", "--help"]))
            .await
            .unwrap(),
        expected_help
    );
    let create_help = call_config_tool(&tools, command(&["datastore", "preview", "create", "-h"]))
        .await
        .unwrap();
    assert!(
        create_help.starts_with("datastore:") && create_help.contains("create ID | update ID"),
        "{create_help}"
    );
    assert!(
        create_help.contains("DatastoreToolSurface fields"),
        "{create_help}"
    );
    assert!(!create_help.contains("Recipe:"), "{create_help}");
    assert_eq!(
        call_config_tool(&tools, command(&["datastore", "help", "create"]))
            .await
            .unwrap(),
        create_help
    );
    for args in [
        vec!["datastore", "create", "--set", "display_name=\"Test\""],
        vec![
            "datastore",
            "preview",
            "create",
            "--set",
            "display_name=\"Test\"",
        ],
    ] {
        let error = call_config_tool(&tools, command(&args)).await.unwrap_err();
        assert!(error.contains("missing SURFACE_ID"), "{error}");
    }
    let preview = call_config_tool(
        &tools,
        command(&[
            "datastore",
            "preview",
            "create",
            "jobs",
            "--set",
            "display_name=\"Jobs\"",
        ]),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&preview).unwrap()["committed"],
        false
    );
    let config_tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let named_preview = config_tool
        .call(
            json!({
                "argv":["datastore","preview","create"],
                "target_id":"jobs",
                "set":{"display_name":"Jobs"}
            })
            .to_string(),
        )
        .await
        .unwrap();
    let mut mailbox_args = json!({
        "argv":["datastore","preview","create"],
        "target_id":"host-attention",
        "options":{"mailbox":{"identity":{"mode":"condition","key":"host-health"},"kind":"flag","action":"ack"}},
        "set":{"enabled":true}
    });
    let mailbox_preview: Value =
        serde_json::from_str(&config_tool.call(mailbox_args.to_string()).await.unwrap()).unwrap();
    assert_eq!(mailbox_preview["committed"], false);
    assert!(
        call_config_tool(&tools, command(&["datastore", "get", "host-attention"]))
            .await
            .is_err()
    );
    mailbox_args["argv"] = json!(["datastore", "create"]);
    config_tool.call(mailbox_args.to_string()).await.unwrap();
    let mailbox_created: Value = serde_json::from_str(
        &call_config_tool(&tools, command(&["datastore", "get", "host-attention"]))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        mailbox_created["document"]["entries"]["entries"][0]["tool_name"],
        "file_mailbox_item"
    );
    assert_eq!(
        mailbox_created["document"]["entries"]["entries"][0]["notification"]["identity"]["key"],
        "host-health"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&named_preview).unwrap(),
        serde_json::from_str::<Value>(&preview).unwrap()
    );
    let error = config_tool
        .call(json!({"argv":["datastore","create"],"set":{"display_name":"Test"}}).to_string())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("missing SURFACE_ID"));
    assert!(
        call_config_tool(&tools, command(&["datastore", "get", "jobs"]))
            .await
            .is_err()
    );
    call_config_tool(
        &tools,
        command(&[
            "datastore",
            "create",
            "jobs",
            "--set",
            "display_name=\"Jobs\"",
        ]),
    )
    .await
    .unwrap();
    assert!(call_config_tool(
        &tools,
        command(&[
            "datastore",
            "create",
            "jobs",
            "--set",
            "display_name=\"Duplicate\"",
        ])
    )
    .await
    .is_err());
    assert!(call_config_tool(
        &tools,
        command(&["datastore", "edit", "jobs", "--set", "node_did=\"foreign\"",])
    )
    .await
    .is_err());
    call_config_tool(
        &tools,
        command(&["datastore", "edit", "jobs", "--set", "tags=[\"user:keep\"]"]),
    )
    .await
    .unwrap();
    let actual = call_config_tool(&tools, command(&["datastore", "get", "jobs"]))
        .await
        .unwrap();
    assert!(actual.contains("Jobs"));
    assert!(actual.contains("user:keep"));
    assert!(!actual.contains("Duplicate"));
    fixture
        .add_collection("type ConfiguredWork { message: String }")
        .await;
    let entries = r#"entries=[{"tool_name":"submit_work","collection":"ConfiguredWork","fields":[{"name":"message","required":true}]}]"#;
    call_config_tool(
        &tools,
        command(&["datastore", "edit", "jobs", "--set", entries]),
    )
    .await
    .unwrap();
    let entry_read = call_config_tool(&tools, command(&["datastore", "get", "jobs"]))
        .await
        .unwrap();
    let entry_read: Value = serde_json::from_str(&entry_read).unwrap();
    assert_eq!(
        entry_read["document"]["entries"]["entries"][0]["tool_name"],
        "submit_work"
    );
    assert!(call_config_tool(
        &tools,
        command(&[
            "datastore",
            "preview",
            "edit",
            "jobs",
            "--set",
            r#"entries=[{"tool_name":"submit_work","collection":"ConfiguredWork","fields":[{"name":"bad-name"}]}]"#
        ])
    )
    .await
    .is_err());
    let core = fixture.core();
    core.apply(tools_request(
        &core,
        vec![(
            "datastore".into(),
            Some(json!({
                "datastore_tool_surface_ids": ["jobs"]
            })),
        )],
    ))
    .await
    .unwrap();
    core.apply(agent_request(
        &core,
        vec![(
            "tags".into(),
            Some(json!([
                crate::tools::builtin::self_config::ENGINEER_AGENT_TAG
            ])),
        )],
    ))
    .await
    .unwrap();
    for verb in [
        &["datastore", "preview", "edit"][..],
        &["datastore", "edit"],
    ] {
        call_config_tool(
            &tools,
            command(&[verb, &["jobs", "--set", "display_name=\"Engineer jobs\""]].concat()),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn structured_config_preview_and_apply_round_trip_literal_prompt() {
    let fixture = SelfConfigFixture::new("structured-config").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["agent"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let read = json!({"argv":["agent","context","get"]}).to_string();
    let before = tool.call(read.clone()).await.unwrap();
    let prompt = "Quoted \"text\"\nActual newline; literal \\n; Unicode λ; {{ doc.message }}";
    let mut request = json!({"argv":["agent","context","preview"],"set":{"system_prompt":prompt}});
    let preview: Value =
        serde_json::from_str(&tool.call(request.to_string()).await.unwrap()).unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["config_execution"]["mutation_entered"], false);
    assert_eq!(tool.call(read.clone()).await.unwrap(), before);
    request["argv"][2] = json!("edit");
    let applied: Value =
        serde_json::from_str(&tool.call(request.to_string()).await.unwrap()).unwrap();
    assert_eq!(applied["config_execution"]["mutation_entered"], true);
    let after: Value = serde_json::from_str(&tool.call(read.clone()).await.unwrap()).unwrap();
    assert_eq!(after["document"]["system_prompt"], prompt);
    request["set"] = json!({"node_did":"foreign"});
    assert!(tool.call(request.to_string()).await.is_err());
    assert_eq!(
        serde_json::from_str::<Value>(&tool.call(read).await.unwrap()).unwrap(),
        after
    );
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

pub(super) async fn call_config_tool(
    tools: &[Box<dyn crate::inference::llm::tool::ToolDyn>],
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

#[tokio::test]
async fn outcome_schema_error_returns_an_executable_schema_recovery() {
    use crate::inference::llm::tool::Tool;
    let fixture = SelfConfigFixture::new("schema-recovery").await;
    fixture.grant_selected_configuration().await;
    fixture
        .add_collection("type Delivery { body: String }")
        .await;
    let admin = crate::ConfigAccess::local_as(
        fixture.db.node.clone(),
        identity::Did::new(fixture.db.node_identity.did()).unwrap(),
    );
    let actor = identity::Did::new(fixture.agent_did()).unwrap();
    for permission in [
        crate::store::config_client::NodePermission::CollectionGet,
        crate::store::config_client::NodePermission::CollectionPatch,
    ] {
        admin
            .grant_node_permission(&actor, permission)
            .await
            .unwrap();
    }
    let tools = fixture.tools(&["automation"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    for call in [
        json!({"argv":["event-source","create","deliveries"],"set":{"source_collection":"Delivery","event_kind":"created"}}),
        json!({"argv":["task","create","handle"],"set":{"prompt_template":"Handle {{ doc.body }}","emit_outcome":true}}),
    ] {
        tool.call(call.to_string()).await.unwrap();
    }
    let trigger = json!({"argv":["trigger","create","on-delivery"],"set":{"task_id":"handle","source":{"kind":"event","event_source_id":"deliveries"}}});
    let error = tool.call(trigger.to_string()).await.unwrap_err();
    let crate::inference::llm::tool::ToolError::ToolCallError(error) = error else {
        panic!("{error}")
    };
    let failure: Value = serde_json::from_str(&error.to_string()).unwrap();
    assert_eq!(failure["recovery"]["tool"], "schema");
    assert_eq!(
        failure["recovery"]["next_call"]["argv"],
        json!(["collection", "preview", "update", "Delivery"])
    );
    let schema = crate::tools::builtin::schema_help::SchemaTool::for_managed_agent(
        fixture.db.node.clone(),
        fixture.agent.as_ref(),
    )
    .unwrap();
    let preview: Value = serde_json::from_str(
        &Tool::call(
            &schema,
            serde_json::from_value(failure["recovery"]["next_call"].clone()).unwrap(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    Tool::call(
        &schema,
        serde_json::from_value(preview["next_call"]["args"].clone()).unwrap(),
    )
    .await
    .unwrap();
    tool.call(trigger.to_string()).await.unwrap();
    let rows = fixture
        .user_access
        .execute("{ Trigger { trigger_id } }")
        .await
        .unwrap();
    assert_eq!(rows["data"]["Trigger"].as_array().unwrap().len(), 1);
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

#[tokio::test]
async fn config_errors_name_the_next_call() {
    let fixture = SelfConfigFixture::new("next-call").await;
    fixture.grant_selected_configuration().await;
    let builder = fixture.install_sibling("Builder").await;
    fixture.grant_node_reader().await;
    let tools = fixture.tools(&["node", "tools"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let failure = |args: Value| async move {
        let error = tool.call(args.to_string()).await.unwrap_err();
        let crate::inference::llm::tool::ToolError::ToolCallError(error) = error else {
            panic!("missing typed config error: {error}");
        };
        serde_json::from_str::<Value>(&error.to_string()).unwrap()
    };

    let skill = failure(json!({"argv":["agent","context","preview"],"options":{"agent":builder},"set":{"skill_ids":["eval-coding-check"]}})).await;
    let message = skill["error"].as_str().unwrap();
    assert!(
        message.starts_with("Skill \"eval-coding-check\" does not exist yet"),
        "{message}"
    );
    assert!(
        message.contains("create it with skill create or import it with skill import"),
        "{message}"
    );
    assert_eq!(
        skill["recovery"]["next_call"],
        json!({"argv":["skill","list"]})
    );
    tool.call(skill["recovery"]["next_call"].to_string())
        .await
        .unwrap();
    assert_eq!(skill["config_execution"]["mutation_entered"], false);

    let tools_ref = failure(json!({"argv":["agent","context","preview"],"options":{"agent":builder},"set":{"tools_id":"proposed-tools"}})).await;
    let message = tools_ref["error"].as_str().unwrap();
    assert!(
        message.contains("create it with its own resource command first"),
        "{message}"
    );
    assert!(!message.contains("plan"), "{message}");
    assert_eq!(
        tools_ref["recovery"]["next_call"],
        json!({"argv":["tools","list"]})
    );
    tool.call(tools_ref["recovery"]["next_call"].to_string())
        .await
        .unwrap();

    let aliased = tool
        .call(json!({"argv":["agent","context","preview","edit"],"options":{"agent":builder},"set":{"skill_ids":[]}}).to_string())
        .await
        .unwrap();
    assert!(aliased.contains("\"committed\": false"), "{aliased}");
    let stray =
        failure(json!({"argv":["agent","context","edit","builder"],"set":{"skill_ids":[]}})).await;
    let message = stray["error"].as_str().unwrap();
    assert!(
        message.contains(r#"agent context edit takes no positional argument "builder""#)
            && message.contains(r#"{"argv":["agent","context","edit"],"options":{"agent":"AGENT_DID"},"set":{"FIELD":VALUE}}"#),
        "{message}"
    );
    assert_eq!(stray["config_execution"]["mutation_entered"], false);

    let named = failure(json!({"argv":["agent","get","Builder"]})).await;
    assert_eq!(
        named["error"],
        format!("unknown agent_did \"Builder\"; did you mean {builder:?}?")
    );
    let core = fixture.core();
    core.apply(agent_request(
        &core,
        vec![("display_name".into(), Some(json!("Night Shift")))],
    ))
    .await
    .unwrap();
    let display = failure(json!({"argv":["agent","get","night shift"]})).await;
    assert_eq!(
        display["error"],
        format!(
            "unknown agent_did \"night shift\"; did you mean {:?}?",
            fixture.agent_did()
        )
    );
    let unknown = failure(json!({"argv":["agent","get","nobody"]})).await;
    assert!(
        unknown["error"]
            .as_str()
            .unwrap()
            .contains(r#"copy an exact DID from ["agent","list"]"#),
        "{unknown}"
    );
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

#[tokio::test]
async fn config_execution_receipts_separate_rejected_syntax_from_write_dispatch() {
    let fixture = SelfConfigFixture::new("config-execution").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&[
        "node",
        "tools",
        "automation",
        "profile",
        "backend",
        "mcp_service",
    ]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    for (args, mutation) in [
        (
            json!({"argv":["agent","create","preview"],"set":{"system_prompt":"literal"}}),
            false,
        ),
        (json!({"argv":["unknown","operation"]}), false),
        (
            json!({"argv":["datastore","create"],"set":{"display_name":"Missing ID"}}),
            false,
        ),
        (
            json!({"argv":["datastore","get"],"target_id":"missing"}),
            false,
        ),
        (
            json!({"argv":["datastore","preview","create"],"target_id":"notifications","set":{"display_name":"Notifications"}}),
            false,
        ),
        (
            json!({"argv":["datastore","create"],"target_id":"notifications","set":{"display_name":"Notifications"}}),
            true,
        ),
        (
            json!({"argv":["datastore","create"],"target_id":"notifications","set":{"display_name":"Duplicate"}}),
            true,
        ),
        (
            json!({"argv":["schema","install"],"options":{"sdl":"type ReceiptProbe { value: String }","digest":"wrong"}}),
            false,
        ),
        (json!({"argv":["schema","install"]}), false),
        (
            json!({"argv":["automation","edit","task"],"target_id":"missing","set":{"display_name":"Missing agent"},"options":{"agent":"missing"}}),
            false,
        ),
    ] {
        let text = match tool.call(args.to_string()).await {
            Ok(text) => text,
            Err(crate::inference::llm::tool::ToolError::ToolCallError(error)) => error.to_string(),
            Err(error) => panic!("missing typed result for {args}: {error}"),
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["config_execution"]["mutation_entered"], mutation,
            "{args}: {value}"
        );
    }
    let read: Value = serde_json::from_str(
        &tool
            .call(json!({"argv":["datastore","get"],"target_id":"notifications"}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(read["config_execution"]["mutation_entered"], false);
    fixture
        .db
        .node
        .shutdown()
        .await
        .expect("shut down test node");
}

#[tokio::test]
async fn agent_category_gates_the_tool() {
    let fixture = SelfConfigFixture::new("agent-gate").await;
    let without_agent = fixture.tools(&["tools"]);
    let error = call_config_tool(&without_agent, vec!["agent".into(), "list".into()])
        .await
        .expect_err("catalog read requires agent grant");
    assert!(error.contains("not granted"), "{error}");

    let with_agent = fixture.tools(&["node"]);
    assert!(with_agent
        .iter()
        .any(|tool| tool.name() == CONFIG_TOOL_NAME));
}

#[tokio::test]
async fn agent_only_grant_cannot_acquire_node_authority_and_writes_require_exact_signer() {
    let fixture = SelfConfigFixture::new("agent-only-owner").await;
    fixture.grant_selected_configuration().await;
    let node = fixture.db.node.clone();
    let owner = fixture.db.node_identity.did().to_owned();
    let current = fixture.agent_did();
    let profile = format!("{current}:inference");
    let tools = fixture.tools(&["agent"]);
    for argv in [
        vec!["agent", "list"],
        vec!["agent", "preview", "default", current],
        vec!["agent", "default", current],
        vec![
            "agent",
            "create",
            "--display-name",
            "Other",
            "--system-prompt",
            "Other instructions",
            "--profile",
            &profile,
        ],
    ] {
        let error = call_config_tool(&tools, argv.into_iter().map(str::to_owned).collect())
            .await
            .expect_err("agent-only authority cannot manage the node catalog");
        assert!(error.contains("category node"), "{error}");
    }
    let edited = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "update".into(),
            current.into(),
            "--set".into(),
            r#"display_name="Renamed""#.into(),
        ],
    )
    .await
    .expect("agent authority can edit its own document");
    let edited: Value = serde_json::from_str(&edited).unwrap();
    assert_eq!(edited["committed"], true);
    let foreign_identity = tokio::task::spawn_blocking(|| node_identity("foreign-signer"))
        .await
        .unwrap();
    let params = agent_params(
        "edit",
        None,
        &[
            "--id".into(),
            current.into(),
            "--display-name".into(),
            "Renamed".into(),
        ],
    )
    .unwrap();
    let rejected = agent_mutate(
        &node,
        &owner,
        foreign_identity.as_ref(),
        &params,
        &Default::default(),
    )
    .await
    .expect_err("foreign signer must fail before publishing configuration");
    assert!(rejected.to_string().contains("exact local node identity"));
}

#[tokio::test]
async fn config_lists_are_bounded_paginated_and_inference_inventory_is_read_only() {
    let fixture = SelfConfigFixture::new("config-inventory").await;
    fixture.install_sibling("beta").await;
    fixture.install_sibling("gamma").await;
    fixture.grant_node_reader().await;
    let tools = fixture.tools(&["node", "profile", "backend"]);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();

    let first: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["agent", "list", "--limit", "1"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["page"]["returned"], 1);
    assert_eq!(first["page"]["truncated"], true);
    let cursor = first["page"]["next_cursor"].as_str().unwrap();
    let second: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["agent", "list", "--limit", "1", "--cursor", cursor]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_ne!(first["agents"][0], second["agents"][0]);

    let agent_did = first["agents"][0]["agent_did"].as_str().unwrap();
    let inspected: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["agent", "get", agent_did]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inspected["agent_did"], agent_did);

    for resource in ["profile", "backend"] {
        let inventory: Value = serde_json::from_str(
            &config
                .call(json!({"argv":[resource, "list", "--limit", "2"]}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(inventory["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        assert!(inventory["note"].as_str().unwrap().contains("read-only"));
        assert!(inventory.to_string().find("\"auth\"").is_none());
    }
}

#[tokio::test]
async fn config_creates_and_discovers_an_unauthenticated_local_backend() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(request.starts_with("GET /v1/models "), "{request}");
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        let body = r#"{"data":[{"id":"fixture-local-model","max_model_len":32768}]}"#;
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });

    let fixture = SelfConfigFixture::new("local-backend-create").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["backend", "profile"]);
    let endpoint = format!("http://{address}/v1");
    let profiles_before: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "profile".into(),
                "list".into(),
                "--limit".into(),
                "50".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let create = vec![
        "backend".into(),
        "preview".into(),
        "create".into(),
        "fixture-local".into(),
        "--endpoint".into(),
        endpoint.clone(),
        "--name".into(),
        "Fixture local".into(),
    ];
    let preview: Value =
        serde_json::from_str(&call_config_tool(&tools, create.clone()).await.unwrap()).unwrap();
    assert_eq!(preview["committed"], false);
    assert!(call_config_tool(
        &tools,
        vec!["backend".into(), "get".into(), "fixture-local".into()]
    )
    .await
    .is_err());
    assert!(call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "preview".into(),
            "create".into(),
            "credential-backend".into(),
            "--endpoint".into(),
            endpoint.clone(),
            "--auth".into(),
            "secret".into(),
        ],
    )
    .await
    .unwrap_err()
    .contains("unknown backend create option --auth"));

    let mut apply = create;
    apply.remove(1);
    let created: Value =
        serde_json::from_str(&call_config_tool(&tools, apply.clone()).await.unwrap()).unwrap();
    assert_eq!(created["committed"], true);
    assert!(call_config_tool(&tools, apply)
        .await
        .unwrap_err()
        .contains("already exists"));

    let discovered: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["backend".into(), "discover".into(), "fixture-local".into()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(discovered["endpoint"], endpoint);
    assert_eq!(discovered["observation"]["probe_status"], "healthy");
    assert_eq!(
        discovered["observation"]["catalog"]["models"][0]["model_name"],
        "fixture-local-model"
    );
    assert!(discovered["note"]
        .as_str()
        .unwrap()
        .contains("did not create a profile"));
    let profiles_after: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "profile".into(),
                "list".into(),
                "--limit".into(),
                "50".into(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(profiles_after["items"], profiles_before["items"]);
    server.await.unwrap();
}

#[tokio::test]
async fn config_targets_owned_working_agent_for_all_bound_documents() {
    let fixture = SelfConfigFixture::new("targeted-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_relation_delete().await;
    let working_did = fixture.install_sibling("working").await;
    fixture.grant_sibling_configuration(&working_did).await;
    let setup = fixture.core();
    setup
        .apply(agent_request(
            &setup,
            vec![(
                "tags".into(),
                Some(json!([
                    crate::tools::builtin::self_config::ENGINEER_AGENT_TAG
                ])),
            )],
        ))
        .await
        .unwrap();
    setup
        .apply(tools_request(
            &setup,
            vec![(
                "self_config".into(),
                Some(json!({"enable_self_config": true})),
            )],
        ))
        .await
        .unwrap();

    let mut tool_config = config(&["node", "agent", "tools", "profile", "backend"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    tool_config.no_lockout = true;
    let tools = fixture.tools_with_config(&tool_config);

    call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "edit".into(),
            working_did.clone(),
            "--set".into(),
            "tags=[\"ui:review\"]".into(),
        ],
    )
    .await
    .unwrap();
    call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "system_prompt=\"Review carefully.\"".into(),
            "--set".into(),
            "skill_ids=[]".into(),
        ],
    )
    .await
    .unwrap();
    call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            format!("--agent={working_did}"),
            "--set".into(),
            "agents={\"target_ids\":[],\"enabled\":false}".into(),
            "--set".into(),
            "remote={\"services\":[]}".into(),
        ],
    )
    .await
    .unwrap();
    call_config_tool(
        &tools,
        vec![
            "profile".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "display_name=\"Working profile\"".into(),
        ],
    )
    .await
    .unwrap();

    let working: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["get".into(), "--agent".into(), working_did.clone()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(working["agent"]["tags"], json!(["ui:review"]));
    assert_eq!(working["context"]["system_prompt"], "Review carefully.");
    assert_eq!(working["documents"]["Tools"]["agents"]["enabled"], false);
    assert!(working["documents"]["Tools"]["remote"]["services"].is_null());
    assert!(
        working["documents"]["Tools"]["self_config"].is_null(),
        "targeting a sibling must protect the invoking Engineer chain without granting config to the sibling"
    );
    let working_core = SelfConfigCore::new(
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        working_did.clone(),
    )
    .unwrap()
    .with_lockout_agent_did(fixture.agent_did().to_owned())
    .with_agent_signer(fixture.agent.managed_agent_signer())
    .with_runtime_identity(Some(fixture.db.node_identity.clone()))
    .with_node_binding(Some(fixture.node_binding.clone()));
    working_core
        .apply(agent_request(
            &working_core,
            vec![(
                "inference_profile_id".into(),
                Some(json!(format!("{}:inference", fixture.agent_did()))),
            )],
        ))
        .await
        .unwrap();
    let shared_backend = call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "enabled=false".into(),
        ],
    )
    .await
    .expect_err("a sibling edit must not disable the invoking Engineer backend");
    assert!(
        shared_backend.contains("backend disabled"),
        "{shared_backend}"
    );
    working_core
        .apply(agent_request(
            &working_core,
            vec![(
                "inference_profile_id".into(),
                Some(json!(format!("{working_did}:inference"))),
            )],
        ))
        .await
        .unwrap();
    assert_eq!(
        working["inference_profile"]["display_name"],
        "Working profile"
    );

    let help = call_config_tool(&tools, vec!["help".into(), "tools".into()])
        .await
        .unwrap();
    assert!(
        help.contains("Fields of Tools:") && help.contains("agents"),
        "{help}"
    );
    let shapes = call_config_tool(&tools, vec!["tools".into(), "edit".into(), "--help".into()])
        .await
        .unwrap();
    assert!(shapes.contains("\"agents\":{\"enabled\""), "{shapes}");
    assert!(shapes.contains("target_ids"), "{shapes}");

    let mailbox_help = call_config_tool(
        &tools,
        vec!["datastore".into(), "create".into(), "--help".into()],
    )
    .await
    .unwrap();
    assert!(
        mailbox_help.contains("{\"key\":\"monitor-summary\",\"mode\":\"condition\"}"),
        "{mailbox_help}"
    );

    let create_profile_args = vec![
        "profile".into(),
        "preview".into(),
        "create".into(),
        "review-profile".into(),
        "--set".into(),
        format!("backend_id=\"{working_did}:backend\""),
        "--set".into(),
        "model_name=\"review-model\"".into(),
        "--set".into(),
        "display_name=\"Review profile\"".into(),
    ];
    let preview: Value = serde_json::from_str(
        &call_config_tool(&tools, create_profile_args.clone())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert!(call_config_tool(
        &tools,
        vec!["profile".into(), "get".into(), "review-profile".into()]
    )
    .await
    .is_err());
    let mut create_profile_args = create_profile_args;
    create_profile_args.remove(1);
    let created: Value = serde_json::from_str(
        &call_config_tool(&tools, create_profile_args.clone())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(created["committed"], true);
    assert!(call_config_tool(&tools, create_profile_args)
        .await
        .unwrap_err()
        .contains("already exists"));
    let profile: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec!["profile".into(), "get".into(), "review-profile".into()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(profile["document"]["model_name"], "review-model");

    let default_preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "preview".into(),
                "default".into(),
                working_did.clone(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(default_preview["committed"], false);
    assert_eq!(default_preview["admitted"], true);
    assert_eq!(default_preview["make_default"], true);

    call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            fixture.agent_did().to_owned(),
            "--set".into(),
            "tags=[\"changed\"]".into(),
        ],
    )
    .await
    .unwrap();
    let lockout = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--set".into(),
            "self_config={\"enable_self_config\":false}".into(),
        ],
    )
    .await
    .unwrap_err();
    assert!(
        lockout.contains("self-config must remain enabled"),
        "{lockout}"
    );

    working_core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![(
                "tools_id".into(),
                Some(json!(format!("{}:tools", fixture.agent_did()))),
            )],
        ))
        .await
        .unwrap();
    let shared_tools = call_config_tool(
        &tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "display_name=\"must not land\"".into(),
        ],
    )
    .await
    .expect_err("Tools shared with Engineer must be protected transactionally");
    assert!(
        shared_tools.contains("unshared Context and Tools"),
        "{shared_tools}"
    );
    working_core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![(
                "tools_id".into(),
                Some(json!(format!("{working_did}:tools"))),
            )],
        ))
        .await
        .unwrap();
    working_core
        .apply(agent_request(
            &working_core,
            vec![(
                "context_id".into(),
                Some(json!(format!("{}:context", fixture.agent_did()))),
            )],
        ))
        .await
        .unwrap();
    let shared_context = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "context".into(),
            "edit".into(),
            "--agent".into(),
            working_did.clone(),
            "--set".into(),
            "display_name=\"must not land\"".into(),
        ],
    )
    .await
    .expect_err("Context shared with Engineer must be protected transactionally");
    assert!(
        shared_context.contains("unshared Context and Tools"),
        "{shared_context}"
    );
}

#[tokio::test]
async fn cleanup_previews_and_removes_exact_unreferenced_cycles_atomically() {
    let fixture = SelfConfigFixture::new("config-cleanup").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_delete().await;
    fixture.grant_agent_relation_delete().await;
    let mut tool_config = config(&["node", "tools", "profile", "backend"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    let tools = fixture.tools_with_config(&tool_config);
    call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "create".into(),
            "orphan-backend".into(),
            "--set".into(),
            "endpoint=\"http://127.0.0.1:1/v1\"".into(),
        ],
    )
    .await
    .expect("Agent creates the removable backend");
    call_config_tool(
        &tools,
        vec![
            "profile".into(),
            "create".into(),
            "orphan-profile".into(),
            "--set".into(),
            "backend_id=\"orphan-backend\"".into(),
            "--set".into(),
            "model_name=\"test-model\"".into(),
        ],
    )
    .await
    .expect("Agent creates the removable profile");
    let created: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "create".into(),
                "--display-name".into(),
                "Orphan".into(),
                "--system-prompt".into(),
                "Orphan fixture".into(),
                "--preset".into(),
                "readonly".into(),
                "--profile".into(),
                "orphan-profile".into(),
            ],
        )
        .await
        .expect("Agent creates the removable agent and configuration"),
    )
    .unwrap();
    let orphan = created["agent_did"].as_str().unwrap().to_owned();

    let referenced = call_config_tool(
        &tools,
        vec![
            "cleanup".into(),
            "preview".into(),
            "--target".into(),
            "backend=orphan-backend".into(),
        ],
    )
    .await
    .expect_err("cleanup must reject a retained profile's backend");
    assert!(
        referenced.contains("references missing InferenceBackend"),
        "{referenced}"
    );

    let targets = [
        format!("agent={orphan}"),
        format!("context={orphan}:context"),
        format!("tools={orphan}:tools"),
        "profile=orphan-profile".to_owned(),
        "backend=orphan-backend".to_owned(),
    ];
    let argv = |verb: &str| {
        let mut argv = vec!["cleanup".to_owned(), verb.to_owned()];
        for target in &targets {
            argv.push("--target".to_owned());
            argv.push(target.clone());
        }
        argv
    };
    let preview: Value = serde_json::from_str(
        &call_config_tool(&tools, argv("preview"))
            .await
            .expect("complete unreferenced cycle previews"),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["targets"].as_array().unwrap().len(), targets.len());
    call_config_tool(&tools, vec!["agent".into(), "get".into(), orphan.clone()])
        .await
        .expect("preview performs no writes");

    call_config_tool(
        &tools,
        vec![
            "backend".into(),
            "edit".into(),
            "--agent".into(),
            orphan.clone(),
            "--set".into(),
            "name=\"Changed after preview\"".into(),
        ],
    )
    .await
    .expect("change one target after preview");
    let mut stale_argv = argv("remove");
    stale_argv.push("--digest".into());
    stale_argv.push(preview["plan_digest"].as_str().unwrap().to_owned());
    let stale = call_config_tool(&tools, stale_argv)
        .await
        .expect_err("cleanup refuses content that changed after preview");
    assert!(stale.contains("preview again"), "{stale}");

    let refreshed: Value = serde_json::from_str(
        &call_config_tool(&tools, argv("preview"))
            .await
            .expect("changed target set can be previewed again"),
    )
    .unwrap();
    let mut remove_argv = argv("remove");
    remove_argv.push("--digest".into());
    remove_argv.push(refreshed["plan_digest"].as_str().unwrap().to_owned());
    let removed: Value = serde_json::from_str(
        &call_config_tool(&tools, remove_argv)
            .await
            .expect("complete unreferenced cycle removes atomically"),
    )
    .unwrap();
    assert_eq!(removed["committed"], true);
    let missing = call_config_tool(&tools, vec!["agent".into(), "get".into(), orphan.clone()])
        .await
        .expect_err("removed agent is no longer inspectable");
    assert!(
        missing.contains("missing")
            || missing.contains("no owned")
            || missing.contains("unknown agent_did"),
        "{missing}"
    );

    call_config_tool(
        &tools,
        vec![
            "get".into(),
            "--agent".into(),
            fixture.agent_did().to_owned(),
        ],
    )
    .await
    .expect("cleanup preserves unrelated configuration");
}

fn take_agent_tool(
    tools: Vec<Box<dyn crate::inference::llm::tool::ToolDyn>>,
) -> Box<dyn crate::inference::llm::tool::ToolDyn> {
    tools
        .into_iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config registered")
}

#[tokio::test]
async fn user_owned_node_default_rejects_agent_update_and_accepts_user() {
    use crate::lean_vocab_test::{LeanIdentityAction, LeanIdentitySubject};
    let model = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "user-node-agents-mixed-session-ownership-policy-followup")
        .unwrap();
    let user = &model
        .objects
        .iter()
        .find(|object| object.resource == "nodes" && object.id == "home")
        .unwrap()
        .owner;
    let agent = model
        .relations
        .iter()
        .find_map(|relation| match &relation.subject {
            LeanIdentitySubject::Actor { actor }
                if relation.resource == "agents"
                    && relation.id == "general"
                    && relation.relation == "executor" =>
            {
                Some(actor)
            }
            _ => None,
        })
        .unwrap();
    let may_update_node = |actor: &str| {
        model
            .actions
            .iter()
            .find_map(|action| match action {
                LeanIdentityAction::Check {
                    actor: candidate,
                    resource,
                    id,
                    permission,
                    expected,
                } if candidate == actor
                    && resource == "nodes"
                    && id == "home"
                    && permission == "update" =>
                {
                    Some(*expected)
                }
                _ => None,
            })
            .unwrap()
    };
    let fixture = SelfConfigFixture::new("agent-default-authority").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let working = fixture.install_sibling("working").await;
    let seed = fixture.agent_did();
    let core = fixture.core();
    let tools = fixture.tools(&["node"]);
    let preview: Value = serde_json::from_str(
        &call_config_tool(
            &tools,
            vec![
                "agent".into(),
                "preview".into(),
                "default".into(),
                working.clone(),
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(
        node_default_agent(&core).await.unwrap().as_deref(),
        Some(seed)
    );
    let agent_update = call_config_tool(
        &tools,
        vec!["agent".into(), "default".into(), working.clone()],
    )
    .await;
    assert_eq!(agent_update.is_ok(), may_update_node(agent));
    assert_eq!(
        node_default_agent(&core).await.unwrap().as_deref(),
        Some(seed)
    );
    fixture
        .update_signed_configuration(
            crate::Collection::Node,
            fixture.node_did(),
            json!({"default_agent_did": working}),
        )
        .await;
    assert!(may_update_node(user));
    assert_eq!(
        node_default_agent(&core).await.unwrap().as_deref(),
        Some(working.as_str())
    );
}

#[tokio::test]
async fn agent_create_commits_and_resolves_after_restart() {
    let fixture = SelfConfigFixture::new("agent-create").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let node = fixture.db.node.clone();
    let node_did = fixture.node_did().to_owned();
    let profile_id = format!("{}:inference", fixture.agent_did());
    let tools = fixture.tools(&["node"]);
    let rejected_preview = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "preview".into(),
            "create".into(),
            "--display-name".into(),
            "Research Assistant".into(),
            "--preset".into(),
            "write".into(),
            "--profile".into(),
            profile_id.clone(),
        ],
    )
    .await
    .expect("invalid preview is an inspectable no-write result");
    let rejected_preview: Value = serde_json::from_str(&rejected_preview).unwrap();
    assert_eq!(rejected_preview["committed"], false);
    assert_eq!(rejected_preview["admitted"], false);
    assert!(rejected_preview["rejection"]
        .as_str()
        .unwrap()
        .contains("fresh agents require system_prompt"));
    let admitted_preview = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "preview".into(),
            "create".into(),
            "--display-name".into(),
            "Research Assistant".into(),
            "--description".into(),
            "Researches a focused question".into(),
            "--system-prompt".into(),
            "Research the question and cite evidence.".into(),
            "--preset".into(),
            "write".into(),
            "--profile".into(),
            profile_id.clone(),
        ],
    )
    .await
    .expect("complete preview succeeds");
    let admitted_preview: Value = serde_json::from_str(&admitted_preview).unwrap();
    assert_eq!(admitted_preview["committed"], false);
    assert_eq!(admitted_preview["admitted"], true);
    assert_eq!(admitted_preview["operation"], "create");
    assert_eq!(admitted_preview["make_default"], false);
    assert_eq!(
        admitted_preview["agent"]["display_name"],
        "Research Assistant"
    );
    assert_eq!(
        admitted_preview["agent"]["inference_profile_id"],
        profile_id
    );
    assert_eq!(
        admitted_preview["agent"]["context_id"],
        format!(
            "{}:context",
            admitted_preview["agent_did"].as_str().unwrap()
        )
    );
    assert_eq!(crate::list_agents(&node, &node_did).await.unwrap().len(), 1);
    let tool = take_agent_tool(tools);

    let args = json!({"argv": [
        "agent", "create",
        "--display-name", "Research Assistant",
        "--description", "Researches a focused question",
        "--system-prompt", "Research the question and cite evidence.",
        "--preset", "write",
        "--profile", profile_id
    ]})
    .to_string();

    let output = tool.call(args).await.expect("direct agent create commits");
    let agents = crate::list_agents(&node, &node_did)
        .await
        .expect("list agents");
    let created: Vec<_> = agents
        .iter()
        .filter(|agent| agent.agent_did != fixture.agent_did())
        .collect();
    assert_eq!(created.len(), 1, "exactly one new agent materialized");
    assert_eq!(
        created[0].display_name,
        Some("Research Assistant".to_string())
    );
    let context_id = crate::store::graphql::escape_graphql_string(
        created[0]
            .context_id
            .as_deref()
            .expect("created agent has context"),
    );
    let response = fixture.user_access.execute(&format!(
        r#"{{ AgentContext(filter: {{ context_id: {{ _eq: "{context_id}" }} }}) {{ description system_prompt }} }}"#,
    )).await.unwrap();
    let context = &response["data"]["AgentContext"][0];
    assert_eq!(
        context["system_prompt"],
        "Research the question and cite evidence."
    );
    assert_eq!(context["description"], "Researches a focused question");

    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["committed"], true);
    assert_eq!(output["agent_did"], created[0].agent_did);
    let effective: Value = serde_json::from_str(
        &agent_inspect(&fixture.core(), &created[0].agent_did)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        effective["effective_config"]["context"]["system_prompt"],
        "Research the question and cite evidence."
    );
    assert_eq!(
        effective["effective_config"]["tool_grants"]["configured"],
        json!({"lsp": false, "network_mode": "inherit"})
    );
    fixture
        .grant_sibling_configuration(&created[0].agent_did)
        .await;
    let grant_tools = fixture.tools(&["node", "tools"]);
    let selected = call_config_tool(
        &grant_tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            created[0].agent_did.clone(),
            "--set".into(),
            r#"integrations={"lsp":{}}"#.into(),
            "--set".into(),
            r#"host={"files":{"mode":"ReadOnly"},"bash":{"mode":"ReadOnly","network_mode":"disabled"}}"#.into(),
        ],
    )
    .await
    .expect("explicit second operation grants sibling tools");
    let applied: Value = serde_json::from_str(&selected).unwrap();
    assert_eq!(applied["committed"], true);
    let selected = call_config_tool(
        &grant_tools,
        vec!["get".into(), "--agent".into(), created[0].agent_did.clone()],
    )
    .await
    .expect("inspect the selected sibling after the canonical tools patch");
    let selected: Value = serde_json::from_str(&selected).unwrap();
    assert_eq!(
        selected["tool_grants"]["configured"],
        json!({"lsp": true, "network_mode": "disabled"})
    );
    assert_eq!(
        selected["runtime_effective"]["effective"]["network_mode"],
        "disabled"
    );

    fixture
        .update_signed_configuration(
            crate::Collection::Node,
            fixture.node_did(),
            json!({"default_agent_did": created[0].agent_did}),
        )
        .await;
    let snapshot = crate::runtime::agent::resolve_document_runtime_snapshot(
        &node,
        &crate::runtime::agent::DocumentResolveContext::for_tests(
            fixture.db.node_identity.clone(),
            crate::tools::surface::ToolCeiling::readonly(),
            Default::default(),
        ),
    )
    .await
    .expect("restart-style runtime resolution accepts the materialized agent");
    assert_eq!(snapshot.default_agent_did, created[0].agent_did);
    let runtime_agent = snapshot
        .agents
        .get(&created[0].agent_did)
        .unwrap_or_else(|| {
            panic!(
                "new default is runnable after re-resolution: {:?}",
                snapshot.unavailable_agents
            )
        });
    assert_eq!(
        runtime_agent.system_prompt,
        "Research the question and cite evidence."
    );
    let names = runtime_agent
        .tools
        .resolve(&node, &node_did, &Default::default())
        .await
        .expect("new agent tool surface resolves after restart")
        .tool_names();
    assert!(names.iter().any(|name| name == "lsp"), "{names:?}");
    assert!(
        !names.iter().any(|name| name == CONFIG_TOOL_NAME),
        "{names:?}"
    );
}

#[tokio::test]
async fn agent_clone_accepts_sibling_agent_did() {
    let fixture = SelfConfigFixture::new("agent-clone").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let node = &fixture.db.node;
    let node_did = fixture.node_did();
    let qualified_sibling_id = fixture.install_sibling("sibling-agent").await;
    let profile_id = format!("{qualified_sibling_id}:inference");
    let tools = fixture.tools(&["node"]);
    let tool = take_agent_tool(tools);
    let args = json!({"argv": [
        "agent", "clone",
        "--display-name", "Cloned Agent",
        "--from", qualified_sibling_id,
        "--profile", profile_id
    ]})
    .to_string();
    let output: Value =
        serde_json::from_str(&tool.call(args).await.expect("clone commits")).unwrap();
    assert_eq!(output["committed"], true);
    let source = crate::list_agents(node, node_did)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.agent_did == qualified_sibling_id)
        .unwrap();
    let cloned = crate::list_agents(node, node_did)
        .await
        .unwrap()
        .into_iter()
        .find(|a| Some(a.agent_did.as_str()) == output["agent_did"].as_str())
        .unwrap();
    assert_eq!(source.inference_profile_id, cloned.inference_profile_id);
    assert_ne!(source.context_id, cloned.context_id);
}

#[tokio::test]
async fn agent_clone_cannot_copy_unheld_grants() {
    let fixture = SelfConfigFixture::new("agent-clone-grants").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let node = &fixture.db.node;
    let node_did = fixture.node_did();
    let granted = fixture.install_sibling("granted").await;
    fixture.grant_pack_install_to(&granted).await;
    let profile = format!("{granted}:inference");
    let mut tool_config = config(&["node"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    let tools = fixture.tools_with_config(&tool_config);
    let tool = take_agent_tool(tools);
    for argv in [
        json!([
            "agent",
            "clone",
            "--display-name",
            "Granted Copy",
            "--from",
            granted,
            "--profile",
            profile
        ]),
        json!([
            "agent",
            "preview",
            "clone",
            "--display-name",
            "Granted Copy",
            "--from",
            granted,
            "--profile",
            profile
        ]),
    ] {
        let refused = if argv[1] == "preview" {
            let output: Value =
                serde_json::from_str(&tool.call(json!({"argv": argv}).to_string()).await.unwrap())
                    .unwrap();
            assert_eq!(output["admitted"], false);
            assert_eq!(output["committed"], false);
            output["rejection"].as_str().unwrap().to_owned()
        } else {
            config_error_message(
                tool.call(json!({"argv": argv}).to_string())
                    .await
                    .expect_err("a clone must not copy grants the invoking agent does not hold"),
            )
        };
        assert!(
            refused.contains(&format!(
                "clone source {granted:?} carries an operator grant this agent does not hold"
            )),
            "{argv}: {refused}"
        );
        assert!(
            refused.contains("cannot be self-granted"),
            "{argv}: {refused}"
        );
    }
    assert!(
        crate::list_agents(node, node_did)
            .await
            .unwrap()
            .iter()
            .all(|agent| agent.display_name.as_deref() != Some("Granted Copy")),
        "the refused clone must not publish an Agent"
    );
}

#[tokio::test]
async fn agent_clone_by_holder_copies_granted_source() {
    let fixture = SelfConfigFixture::new("agent-clone-holder").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let node = &fixture.db.node;
    let node_did = fixture.node_did();
    let granted = fixture.install_sibling("granted").await;
    fixture.grant_pack_install_to(&granted).await;
    let profile = format!("{granted}:inference");
    fixture.grant_pack_install().await;
    let mut tool_config = config(&["node"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.preview = true;
    tool_config.enable_pack_install = true;
    let tools = fixture.tools_with_config(&tool_config);
    let tool = take_agent_tool(tools);
    tool.call(
        json!({"argv": ["agent", "preview", "clone", "--display-name", "Granted Copy",
                        "--from", granted, "--profile", profile]})
        .to_string(),
    )
    .await
    .expect("a holder may preview copying a granted source");
    let args = json!({"argv": ["agent", "clone", "--display-name", "Granted Copy",
                               "--from", granted, "--profile", profile]})
    .to_string();
    let output: Value =
        serde_json::from_str(&tool.call(args).await.expect("a holder's clone commits")).unwrap();
    assert_eq!(output["committed"], true);
    assert!(
        crate::list_agents(node, node_did)
            .await
            .unwrap()
            .iter()
            .any(|agent| Some(agent.agent_did.as_str()) == output["agent_did"].as_str()),
        "a holder's clone publishes its Agent"
    );
}

#[tokio::test]
async fn canonical_self_config_preview_and_apply_preserve_scope_and_reject_lockout() {
    let fixture = SelfConfigFixture::new("canonical-self-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let sibling = fixture.install_sibling("unconfigured").await;
    let other = tokio::task::spawn_blocking(|| node_identity("other-self-config"))
        .await
        .unwrap();
    let foreign = other.did().to_owned();
    crate::test_support::bootstrap_signed_test_node(fixture.user_access(), &foreign).await;
    let tools_id = format!("{}:tools", fixture.agent_did());
    let document = json!({"node_did": foreign, "tools_id": tools_id});
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::Tools,
            add: document.clone(),
            update: document,
        },
    ])
    .unwrap();
    fixture
        .user_access()
        .transact_write("test.foreign_tools", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
    let core = fixture.core();
    let patch = vec![(
        "self_config".into(),
        Some(json!({"enable_self_config":true})),
    )];
    let preview = core
        .preview(tools_request(&core, patch.clone()))
        .await
        .unwrap();
    assert!(!preview.committed);
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(read["documents"]["Tools"]["self_config"].is_null());
    core.apply(tools_request(&core, patch)).await.unwrap();
    let guarded = core.clone().with_no_lockout(true);
    assert!(guarded
        .apply(tools_request(&guarded, vec![("self_config".into(), None)],))
        .await
        .is_err());
    assert!(guarded
        .preview(agent_request(
            &guarded,
            vec![(
                "context_id".into(),
                Some(json!(format!("{sibling}:context")))
            )]
        ))
        .await
        .is_err());
    assert!(core
        .apply(anchored_request(
            SelfConfigTarget::AgentContext,
            "context_id",
            vec![("tools_id".into(), Some(json!("missing")))]
        ))
        .await
        .is_err());
    assert!(core
        .preview(tools_request(
            &core,
            vec![("host".into(), Some(json!({"unexpected":true})))],
        ))
        .await
        .is_err());
    let own = core
        .read_effective_config(&BTreeSet::new(), true, true)
        .await
        .unwrap();
    assert_eq!(own["context"]["tools_id"], tools_id);
    assert_eq!(
        own["documents"]["Tools"]["self_config"]["enable_self_config"],
        true
    );
    let foreign_read = fixture.user_access()
        .execute(&format!(r#"{{ Tools(filter: {{node_did: {{_eq: "{}"}}, tools_id: {{_eq: "{}"}}}}) {{ self_config }} }}"#,
            escape_graphql_string(&foreign), escape_graphql_string(&tools_id)))
        .await
        .unwrap();
    let foreign_tools = foreign_read["data"]["Tools"].as_array().unwrap();
    assert_eq!(foreign_tools.len(), 1);
    assert!(foreign_tools[0]["self_config"].is_null());
}

#[tokio::test]
async fn direct_tools_preview_and_apply_enforce_and_persist_canonical_workspace_root() {
    let fixture = SelfConfigFixture::new("root-policy").await;
    fixture.grant_selected_configuration().await;
    let owner = fixture.node_did().to_owned();
    let agent_did = fixture.agent_did().to_owned();
    let access = fixture.user_access();
    let ceiling = tempfile::tempdir().unwrap();
    let selected = ceiling.path().join("selected");
    let sibling = ceiling.path().join("sibling");
    std::fs::create_dir_all(&selected).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let selected_text = selected.to_string_lossy();
    crate::tools::surface::set_workspace_root_with_readers(
        access,
        &owner,
        &selected_text,
        None,
        true,
        std::slice::from_ref(&agent_did),
    )
    .await
    .unwrap();
    let core =
        fixture
            .core()
            .with_process_ceiling(crate::tools::surface::SelfConfigProcessCeiling {
                file_mode: crate::tools::surface::FileToolMode::ReadOnly,
                bash_mode: crate::tools::surface::BashMode::Off,
                root: Some(ceiling.path().to_path_buf()),
            });
    let patch = |root: &std::path::Path| {
        vec![(
            "host".into(),
            Some(json!({
                "root": root.to_string_lossy(),
                "files": {"mode": "ReadOnly"}
            })),
        )]
    };

    let authored_inside = selected.join("detour").join("..");
    let preview = core
        .preview(tools_request(&core, patch(&authored_inside)))
        .await
        .expect("preview admits a descendant and does not persist it");
    assert!(!preview.committed);
    assert!(core
        .preview(tools_request(&core, patch(&sibling)))
        .await
        .is_err());
    assert!(core
        .apply(tools_request(&core, patch(&sibling)))
        .await
        .is_err());

    core.apply(tools_request(&core, patch(&authored_inside)))
        .await
        .expect("apply admits the selected root");
    let tools_id = format!("{agent_did}:tools");
    let persisted: crate::documents::Tools = core
        .access()
        .unwrap()
        .transact_write("test.self_config.read_tools", |txn| {
            let owner = owner.clone();
            let tools_id = tools_id.clone();
            Box::pin(async move {
                let value = crate::store::config_client::read_desired_state_document_in_txn(
                    txn,
                    crate::Collection::Tools,
                    &owner,
                    &tools_id,
                )
                .await?
                .context("persisted Tools")?;
                Ok::<_, anyhow::Error>(serde_json::from_value(value)?)
            })
        })
        .await
        .unwrap();
    assert_eq!(
        persisted
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(
            std::fs::canonicalize(&selected)
                .unwrap()
                .to_string_lossy()
                .as_ref()
        )
    );
}

#[tokio::test]
async fn descendant_root_preview_apply_reconcile_reaches_fresh_request_file_tools() {
    let fixture = SelfConfigFixture::new("descendant-root-self-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let owner = fixture.node_did().to_owned();
    let seed_agent = fixture.agent_did().to_owned();
    let operator = fixture.user_access();

    let operator_root = tempfile::tempdir().expect("operator root");
    let selected_root = operator_root.path().join("projects").join("mandrake");
    std::fs::create_dir_all(&selected_root).expect("selected descendant root");
    std::fs::write(
        selected_root.join("marker.txt"),
        "fresh request sees descendant\n",
    )
    .expect("marker file");
    let sibling_root = operator_root.path().join("projects").join("other");
    std::fs::create_dir_all(&sibling_root).expect("sibling root");
    let sibling_marker = sibling_root.join("not-authorized.txt");
    std::fs::write(
        &sibling_marker,
        "operator ceiling must not widen selection\n",
    )
    .expect("sibling marker");
    let original_transcript = operator_root.path().join("original-transcript.txt");
    std::fs::write(&original_transcript, "private transcript fixture\n")
        .expect("original transcript fixture");

    let selected_text = selected_root.to_string_lossy().into_owned();
    crate::tools::surface::set_workspace_root_with_readers(
        operator,
        &owner,
        &selected_text,
        None,
        true,
        std::slice::from_ref(&seed_agent),
    )
    .await
    .expect("publish selected WorkspaceRoot");

    let mut tool_config = config(&["node"]);
    tool_config.agent_did = seed_agent.clone();
    tool_config.process_ceiling = crate::tools::surface::SelfConfigProcessCeiling {
        file_mode: crate::tools::surface::FileToolMode::ReadOnly,
        bash_mode: crate::tools::surface::BashMode::Off,
        root: Some(operator_root.path().to_path_buf()),
    };
    let agent_tools = fixture.tools_with_config(&tool_config);
    let profile_id = format!("{seed_agent}:inference");
    let command = |preview: bool| {
        let mut argv = vec!["agent".to_string()];
        if preview {
            argv.push("preview".to_string());
        }
        argv.extend([
            "create".to_string(),
            "--display-name".to_string(),
            "Mandrake descendant".to_string(),
            "--system-prompt".to_string(),
            "Use only the selected workspace.".to_string(),
            "--root".to_string(),
            selected_text.clone(),
            "--preset".to_string(),
            "readonly".to_string(),
            "--profile".to_string(),
            profile_id.clone(),
        ]);
        argv
    };
    let preview: Value = serde_json::from_str(
        &call_config_tool(&agent_tools, command(true))
            .await
            .expect("agent preview accepts the published descendant"),
    )
    .expect("preview json");
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["admitted"], true);
    assert_eq!(crate::list_agents(&node, &owner).await.unwrap().len(), 1);
    let output = call_config_tool(&agent_tools, command(false))
        .await
        .expect("direct create commits");
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["committed"], true);
    let agents = crate::list_agents(&node, &owner)
        .await
        .expect("list materialized agents");
    let created = agents
        .iter()
        .find(|agent| agent.agent_did != seed_agent)
        .expect("agent reconciler materialized one agent");
    let created_signer = crate::store::identity::resolve_agent_identity(
        identity.as_ref(),
        &created.identity_binding(),
    )
    .expect("created Agent is derived from the held runtime");
    let created_access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        ::identity::Did::new(&created.agent_did).unwrap(),
        created_signer.as_ref(),
    )
    .unwrap();
    fixture
        .update_signed_configuration(
            crate::Collection::Node,
            &owner,
            json!({"default_agent_did": created.agent_did}),
        )
        .await;
    crate::tools::surface::grant_workspace_root_reader(
        operator,
        &selected_text,
        &created.agent_did,
    )
    .await
    .expect("User grants the created Agent access to the selected root");
    let runtime_view =
        crate::runtime::agent::document_view::load_document_runtime_view(&node, &owner)
            .await
            .expect("fresh runtime view after agent publication");
    let context_id = created.context_id.as_ref().expect("created context");
    let tools_id = runtime_view.contexts[context_id]
        .value
        .tools_id
        .as_ref()
        .expect("created tools");
    let canonical_selected =
        std::fs::canonicalize(&selected_root).expect("canonical selected root");
    assert_eq!(
        runtime_view.tools[tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(canonical_selected.to_string_lossy().as_ref()),
        "agent apply must persist the canonical selected root, not the operator ceiling"
    );

    let snapshot = crate::runtime::agent::resolve_document_runtime_snapshot(
        &node,
        &crate::runtime::agent::DocumentResolveContext::for_tests(
            identity.clone(),
            crate::tools::surface::ToolCeiling::readonly_at(operator_root.path()),
            Default::default(),
        ),
    )
    .await
    .expect("fresh request runtime snapshot");
    let runtime_agent = snapshot
        .agents
        .get(&created.agent_did)
        .expect("fresh request agent is runnable")
        .clone();
    let surface = Arc::new(
        runtime_agent
            .tools
            .resolve(&node, &owner, &Default::default())
            .await
            .expect("resolve fresh-request tool surface"),
    );
    let runtime = crate::tools::surface::ToolRuntimeContext::oneshot_with_node_did(
        node.clone(),
        owner.clone(),
    );

    let cases = [
        (
            "selected descendant",
            "marker.txt".to_string(),
            "fresh request sees descendant",
            None,
        ),
        (
            "sibling under operator ceiling",
            sibling_marker.to_string_lossy().into_owned(),
            "outside the allowed tool root",
            Some("operator ceiling must not widen selection"),
        ),
        (
            "original transcript path",
            original_transcript.to_string_lossy().into_owned(),
            "outside the allowed tool root",
            Some("private transcript fixture"),
        ),
    ];
    for (label, path, expected_result, forbidden_content) in cases {
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = crate::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            crate::request::lifecycle::RequestIdentity {
                requester_did: Some(created.agent_did.clone()),
                request_id: request_id.clone(),
                node_did: owner.clone(),
                agent_did: created.agent_did.clone(),
                session_id: uuid::Uuid::new_v4().to_string(),
                content: "Read the requested workspace path.".to_string(),
                execution_origin: crate::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                &created.agent_did,
            ),
        );
        let request = crate::request::lifecycle::build_signed_client_request(
            &created_access,
            request,
            crate::request::lifecycle::RequestSigner::Identity(created_signer.as_ref()),
        )
        .await
        .expect("reserve and sign a fresh request for the reconciled Agent");
        crate::request::lifecycle::submit_client_request(&created_access, &request)
            .await
            .expect("publish the fresh request and its session");
        let response = created_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                escape_graphql_string(&request_id),
            ))
            .await
            .unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let request_doc_id = rows[0]["_docID"].as_str().unwrap().to_owned();
        let request = crate::request::admission::load_request_for_admission_test(
            node.as_ref(),
            &request_doc_id,
        )
        .await
        .expect("load the fresh request through the admission representation");
        assert_eq!(request.agent_did, created.agent_did);
        let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));

        crate::runtime::agent::process_owned_request_with_model_for_test(
            node.clone(),
            runtime_agent.clone(),
            surface.clone(),
            &runtime,
            RootReadModel {
                path,
                turns: turns.clone(),
                provider_inputs: provider_inputs.clone(),
            },
            request,
        )
        .await
        .expect("run the persisted request through the production owned loop");
        let escaped_request_doc = crate::store::graphql::escape_graphql_string(&request_doc_id);
        let observed = operator
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{_docID: {{_eq:"{escaped_request_doc}"}}}}) {{lifecycle_state failure_reason terminal_output}} }}"#
            ))
            .await;
        let data = &observed.expect("owned-loop observations")["data"];
        assert!(
            turns.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "{label}: provider did not receive the tool-result turn; request: {}",
            data["AgentRequest"][0]
        );
        assert!(
            provider_inputs
                .lock()
                .expect("provider inputs")
                .iter()
                .any(|input| input.contains(expected_result)),
            "{label}: provider did not receive the expected tool result/denial: {:?}",
            provider_inputs.lock().expect("provider inputs")
        );
        if let Some(forbidden_content) = forbidden_content {
            assert!(
                provider_inputs
                    .lock()
                    .expect("provider inputs")
                    .iter()
                    .all(|input| !input.contains(forbidden_content)),
                "{label}: denied file contents reached provider input: {:?}",
                provider_inputs.lock().expect("provider inputs")
            );
        }

        assert_eq!(
            data["AgentRequest"][0]["lifecycle_state"], "completed",
            "{label}: fresh owned request did not complete"
        );
    }
    let persisted_after_requests =
        crate::runtime::agent::document_view::load_document_runtime_view(&node, &owner)
            .await
            .expect("reload persisted Tools after fresh owned requests");
    assert_eq!(
        persisted_after_requests.tools[tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(canonical_selected.to_string_lossy().as_ref()),
        "fresh request acceptance must not widen persisted/effective root"
    );
}

#[tokio::test]
async fn backend_self_config_protects_raw_keys_in_writes_reads_and_diffs() {
    let fixture = SelfConfigFixture::new("self-config-secret").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_selected_backend_writer().await;
    let core = fixture.core();
    let raw = vec![(
        "auth".into(),
        Some(json!({"kind":"api_key","key":"do-not-expose"})),
    )];
    assert!(core.preview(backend_request(raw.clone())).await.is_err());
    assert!(core.apply(backend_request(raw)).await.is_err());
    let model = crate::lean_vocab_test::lean_self_config_cases()
        .iter()
        .find(|case| case.name == "backend_credential_to_raw_key_rejected")
        .unwrap();
    let stored: Value = serde_json::from_str(
        &model
            .doc
            .iter()
            .find(|field| field.field == "auth")
            .unwrap()
            .value,
    )
    .unwrap();
    let raw_key: Value = serde_json::from_str(
        model
            .patch
            .iter()
            .find(|field| field.field == "auth")
            .unwrap()
            .value
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    let backend: crate::InferenceBackend = serde_json::from_value(json!({
        "node_did": fixture.node_did(), "backend_id": format!("{}:backend", fixture.agent_did()),
        "name": "Test inference", "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:1/v1", "auth": stored,
    }))
    .unwrap();
    let secret = crate::inference::auth::credential::BackendCredential {
        credential_id: stored["id"].as_str().unwrap().to_owned(),
        node_did: fixture.node_did().to_owned(),
        provider_kind: backend.provider_kind.as_str().to_owned(),
        endpoint: backend.endpoint.clone(),
        version: stored["version"].as_str().unwrap().to_owned(),
        secret: crate::inference::auth::credential::BackendCredentialSecret::ApiKey {
            key: raw_key["key"].as_str().unwrap().to_owned(),
        },
    };
    fixture
        .user_access()
        .transact_write("test.raw_key.credential", |txn| {
            let secret = &secret;
            Box::pin(async move {
                crate::inference::auth::credential::put_backend_credential_in_txn(txn, secret).await
            })
        })
        .await
        .unwrap();
    crate::store::config_client::write_inference_backend_document(fixture.user_access(), &backend)
        .await
        .unwrap();
    for result in [
        core.preview(backend_request(vec![(
            "auth".into(),
            Some(raw_key.clone()),
        )]))
        .await,
        core.apply(backend_request(vec![(
            "auth".into(),
            Some(raw_key.clone()),
        )]))
        .await,
    ] {
        assert_eq!(result.is_ok(), model.accepted);
    }
    assert!(core
        .apply(backend_request(vec![(
            "auth".into(),
            Some(json!({"kind":"api_key","key":"replacement-secret"}))
        )]))
        .await
        .is_err());
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(!read.to_string().contains("operator-secret"));
    let patch = vec![(
        "auth".into(),
        Some(json!({"kind":"environment","variable":"GENTS_TEST_KEY_REFERENCE"})),
    )];
    let preview = core.preview(backend_request(patch)).await.unwrap();
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("operator-secret"));
}

#[tokio::test]
async fn unrelated_backend_patch_keeps_environment_reference_unresolved() {
    const VARIABLE: &str = "GENTS_TEST_UNRELATED_PATCH_ENV_REFERENCE";
    const SENTINEL: &str = "sentinel-env-secret-never-stored";
    let fixture = SelfConfigFixture::new("self-config-env-reference").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_selected_backend_writer().await;
    let core = fixture.core();
    let reference = json!({"kind":"environment","variable":VARIABLE});
    core.apply(backend_request(vec![(
        "auth".into(),
        Some(reference.clone()),
    )]))
    .await
    .unwrap();
    let _variable = crate::test_support::EnvVarGuard::set(VARIABLE, SENTINEL);

    for patch in [
        vec![("name".into(), Some(json!("Renamed inference")))],
        vec![("connect_timeout_secs".into(), Some(json!(17)))],
    ] {
        let preview = core.preview(backend_request(patch.clone())).await.unwrap();
        let applied = core.apply(backend_request(patch)).await.unwrap();
        for outcome in [&preview, &applied] {
            let rendered = serde_json::to_string(outcome).unwrap();
            assert!(!rendered.contains(SENTINEL), "{rendered}");
            assert!(
                !rendered.contains("\"auth\""),
                "an unrelated patch must not report an auth delta: {rendered}"
            );
        }
    }

    let backend_id = escape_graphql_string(&format!("{}:backend", fixture.agent_did()));
    let response = fixture.user_access().execute(&format!(
        r#"{{ InferenceBackendMetadata(filter: {{backend_id: {{_eq: "{backend_id}"}}}}) {{ name connect_timeout_secs auth_kind environment_variable }} }}"#
    )).await.unwrap();
    let stored = &response["data"]["InferenceBackendMetadata"][0];
    assert_eq!(stored["name"], "Renamed inference");
    assert_eq!(stored["connect_timeout_secs"], 17);
    assert_eq!(stored["auth_kind"], "environment");
    assert_eq!(stored["environment_variable"], VARIABLE);
    assert!(!stored.to_string().contains(SENTINEL));
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(!read.to_string().contains(SENTINEL));
    assert!(read.to_string().contains(VARIABLE));
}

#[tokio::test]
async fn backend_self_config_cannot_set_or_change_an_oauth_account() {
    let fixture = SelfConfigFixture::new("self-config-account").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_selected_backend_writer().await;
    let owner = fixture.node_did();
    let backend_id = format!("{}:backend", fixture.agent_did());
    let core = fixture.core();
    let access = fixture.user_access();
    let store = |kind: &str, auth: Value| {
        let backend: crate::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": backend_id, "name": "Test inference",
            "provider_kind": kind, "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap();
        async move {
            crate::store::config_client::write_inference_backend_document(access, &backend)
                .await
                .unwrap();
        }
    };
    let to_oauth = |auth: Value| {
        vec![
            ("provider_kind".into(), Some(json!("ChatGptCodex"))),
            ("auth".into(), Some(auth)),
        ]
    };
    let refused = |patch: SelfConfigPatch| {
        let core = &core;
        async move {
            let preview = core.preview(backend_request(patch.clone())).await;
            let apply = core.apply(backend_request(patch)).await;
            for result in [preview, apply] {
                let error = result.expect_err("account reference change must be refused");
                assert!(
                    format!("{error:#}").contains("OAuth account references are operator-managed"),
                    "{error:#}"
                );
            }
        }
    };

    store(
        "OpenAiCompatible",
        json!({"kind":"environment","variable":"KEY"}),
    )
    .await;
    refused(to_oauth(json!({"kind":"node_oauth","account_ref":"a1"}))).await;
    let original = to_oauth(json!({"kind":"node_oauth"}));
    core.preview(backend_request(original.clone()))
        .await
        .unwrap();
    core.apply(backend_request(original)).await.unwrap();

    store(
        "ChatGptCodex",
        json!({"kind":"node_oauth","account_ref":"a1"}),
    )
    .await;
    refused(to_oauth(json!({"kind":"node_oauth","account_ref":"a2"}))).await;
    refused(to_oauth(json!({"kind":"node_oauth"}))).await;
    let endpoint = vec![("endpoint".into(), Some(json!("http://127.0.0.1:2/v1")))];
    core.preview(backend_request(endpoint.clone()))
        .await
        .unwrap();
    core.apply(backend_request(endpoint)).await.unwrap();
}

#[tokio::test]
async fn backend_choice_uses_runtime_visible_default_in_agent_transaction() {
    let fixture = SelfConfigFixture::new("account-default-choice").await;
    fixture.grant_node_reader().await;
    let owner = fixture.node_did();
    let credential = crate::inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: "model-default-credential".into(),
        node_did: owner.into(),
        provider: crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER.into(),
        access_token: "private-access".into(),
        refresh_token: "private-refresh".into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: Some("g2".into()),
        connected_at: Some(chrono::Utc::now()),
        provider_account_key: None,
        label: None,
    };
    crate::inference::auth::credential::upsert_oauth_credential_on(
        fixture.user_access(),
        &credential,
    )
    .await
    .unwrap();
    let model = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let core = fixture.core();
    let access = core.access().unwrap();
    let mut disabled = false;
    for (index, case) in model.runtime_default_choice_cases.iter().enumerate() {
        if case["visible_default"].is_null() && !disabled {
            crate::inference::auth::credential::set_account_enabled(
                fixture.user_access(),
                owner,
                &credential.credential_id,
                false,
            )
            .await
            .unwrap();
            disabled = true;
        }
        let current_id = format!("guard-current-{index}");
        let next_id = format!("guard-next-{index}");
        for (id, kind, account) in [
            (&current_id, &case["current_kind"], &case["current_ref"]),
            (&next_id, &case["next_kind"], &case["next_ref"]),
        ] {
            let backend: crate::InferenceBackend = serde_json::from_value(json!({
                "node_did": owner, "backend_id": id, "name": id,
                "provider_kind": kind, "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind":"node_oauth", "account_ref": account},
            }))
            .unwrap();
            crate::store::config_client::write_inference_backend_document(
                fixture.user_access(),
                &backend,
            )
            .await
            .unwrap();
        }
        let result = access
            .transact_readonly("test.account_choice.runtime_default", |txn| {
                let (current_id, next_id) = (&current_id, &next_id);
                Box::pin(async move {
                    ops::guard_backend_choice_in_txn(txn, owner, Some(current_id), next_id).await
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{}: {result:?}",
            case["name"]
        );
        if let Err(error) = result {
            assert!(!format!("{error:#}").contains("g2"));
        }
    }
}

async fn account_choice_core(name: &str) -> (SelfConfigFixture, SelfConfigCore) {
    let fixture = SelfConfigFixture::new(name).await;
    fixture.grant_node_reader().await;
    fixture.grant_user_relation_delete().await;
    fixture.grant_agent_relation_delete().await;
    let owner = fixture.node_did().to_owned();
    let agent = fixture.agent_did().to_owned();
    let access = fixture.user_access();
    for (id, kind, auth) in [
        (
            "chat-original",
            "ChatGptCodex",
            json!({"kind":"node_oauth"}),
        ),
        (
            "chat-b",
            "ChatGptCodex",
            json!({"kind":"node_oauth","account_ref":"acct-b"}),
        ),
        (
            "grok-original",
            "XaiGrokOAuth",
            json!({"kind":"node_oauth"}),
        ),
    ] {
        let backend: crate::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": id, "name": id, "provider_kind": kind,
            "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap();
        crate::store::config_client::write_inference_backend_document(access, &backend)
            .await
            .unwrap();
    }
    for (id, backend) in [
        (format!("{agent}:inference"), "chat-original"),
        ("p-original".into(), "chat-original"),
        ("p-chat-b".into(), "chat-b"),
        ("p-grok".into(), "grok-original"),
    ] {
        let profile: crate::documents::InferenceProfile = serde_json::from_value(json!({
            "node_did": owner, "profile_id": id, "backend_id": backend,
            "model_name": "test-model",
        }))
        .unwrap();
        crate::store::config_client::write_inference_profile_document(access, &profile)
            .await
            .unwrap();
    }
    let compaction = format!("{agent}:compaction");
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(
        [
            (
                crate::Collection::Compaction,
                json!({"node_did": owner, "compaction_id": compaction}),
            ),
            (
                crate::Collection::AgentContext,
                json!({"node_did": owner, "context_id": format!("{agent}:context"),
                    "tools_id": format!("{agent}:tools"), "compaction_id": compaction}),
            ),
        ]
        .into_iter()
        .chain(["original", "chat-b"].into_iter().flat_map(|account| {
            let compaction = format!("c-{account}");
            [
                (
                    crate::Collection::Compaction,
                    json!({"node_did": owner, "compaction_id": compaction,
                        "inference_profile_id": format!("p-{account}")}),
                ),
                (
                    crate::Collection::AgentContext,
                    json!({"node_did": owner, "context_id": format!("ctx-{account}"),
                        "compaction_id": compaction}),
                ),
            ]
        }))
        .map(
            |(collection, value)| crate::store::config_client::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect(),
    )
    .unwrap();
    access
        .transact_write("test.compaction", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
    fixture.grant_selected_configuration().await;
    let mut selections = vec![(crate::Collection::Compaction, compaction, true)];
    selections.push((
        crate::Collection::AgentContext,
        format!("{agent}:context"),
        false,
    ));
    selections.extend(
        ["p-original", "p-chat-b", "p-grok"]
            .into_iter()
            .map(|id| (crate::Collection::InferenceProfile, id.to_owned(), false)),
    );
    selections.extend(
        ["c-original", "c-chat-b"]
            .into_iter()
            .map(|id| (crate::Collection::Compaction, id.to_owned(), false)),
    );
    selections.extend(
        ["ctx-original", "ctx-chat-b"]
            .into_iter()
            .map(|id| (crate::Collection::AgentContext, id.to_owned(), false)),
    );
    selections.extend(
        ["chat-original", "chat-b", "grok-original"]
            .into_iter()
            .map(|id| (crate::Collection::InferenceBackend, id.to_owned(), false)),
    );
    fixture.grant_account_fixture_selections(&selections).await;
    let core = fixture.core();
    (fixture, core)
}

async fn assert_account_choice_refused(
    core: &SelfConfigCore,
    request: impl Fn() -> ApplyRequest<'static>,
) {
    for result in [core.preview(request()).await, core.apply(request()).await] {
        let error = result.expect_err("switching to another account must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn profile_self_config_cannot_pick_another_account() {
    let (_fixture, core) = account_choice_core("pick").await;
    let owner = core.node_did().to_owned();
    let to = |backend: &str| vec![("backend_id".into(), Some(json!(backend)))];
    assert_account_choice_refused(&core, || profile_request(to("chat-b"))).await;
    core.preview(profile_request(to("grok-original")))
        .await
        .unwrap();
    core.apply(profile_request(to("grok-original")))
        .await
        .unwrap();
    assert_account_choice_refused(&core, || {
        let mut patch = to("chat-b");
        patch.push(("model_name".into(), Some(json!("test-model"))));
        profile_create_request(owner.clone(), "p-new".into(), patch)
    })
    .await;
}

#[tokio::test]
async fn agent_profile_pick_cannot_switch_account() {
    let (_fixture, core) = account_choice_core("agent-pick").await;
    let to = |profile: &str| vec![("inference_profile_id".into(), Some(json!(profile)))];
    assert_account_choice_refused(&core, || agent_request(&core, to("p-chat-b"))).await;
    core.apply(agent_request(&core, to("p-original")))
        .await
        .unwrap();
    core.preview(agent_request(&core, to("p-grok")))
        .await
        .unwrap();
    core.apply(agent_request(&core, to("p-grok")))
        .await
        .unwrap();
}

async fn grok_accounts_core(
    agent: &str,
    original: bool,
    g2: bool,
) -> (SelfConfigFixture, SelfConfigCore) {
    let (fixture, core) = account_choice_core(agent).await;
    let owner = core.node_did().to_owned();
    let access = fixture.user_access();
    let backend: crate::InferenceBackend = serde_json::from_value(json!({
        "node_did": owner, "backend_id": "grok-g2", "name": "grok-g2",
        "provider_kind": "XaiGrokOAuth", "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind":"node_oauth","account_ref":"g2"},
    }))
    .unwrap();
    crate::store::config_client::write_inference_backend_document(access, &backend)
        .await
        .unwrap();
    let profile: crate::documents::InferenceProfile = serde_json::from_value(json!({
        "node_did": owner, "profile_id": "p-grok-g2", "backend_id": "grok-g2",
        "model_name": "test-model",
    }))
    .unwrap();
    crate::store::config_client::write_inference_profile_document(access, &profile)
        .await
        .unwrap();
    fixture
        .grant_account_fixture_selections(&[
            (crate::Collection::InferenceBackend, "grok-g2".into(), false),
            (
                crate::Collection::InferenceProfile,
                "p-grok-g2".into(),
                false,
            ),
        ])
        .await;
    let provider = crate::inference::clients::xai_grok::XAI_OAUTH_PROVIDER;
    for (account_ref, enabled) in [(None, original), (Some("g2"), g2)] {
        let original_id = crate::inference::auth::credential::oauth_credential_id(&owner, provider);
        let credential = crate::inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: match account_ref {
                Some(account_ref) => format!("{original_id}:{account_ref}"),
                None => original_id,
            },
            node_did: owner.clone(),
            provider: provider.to_string(),
            access_token: "access-TEST".into(),
            refresh_token: "refresh-TEST".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled,
            account_ref: account_ref.map(str::to_string),
            connected_at: account_ref.map(|_| chrono::Utc::now()),
            provider_account_key: None,
            label: None,
        };
        crate::inference::auth::credential::upsert_oauth_credential_on(access, &credential)
            .await
            .unwrap();
    }
    (fixture, core)
}

#[tokio::test]
async fn default_account_follows_the_resolver() {
    let to = |profile: &str| vec![("inference_profile_id".into(), Some(json!(profile)))];
    let accepted = |core: SelfConfigCore, profile: &'static str| async move {
        core.preview(agent_request(&core, to(profile)))
            .await
            .unwrap();
        core.apply(agent_request(&core, to(profile))).await.unwrap();
    };

    let (_fixture, core) = grok_accounts_core("default-g2", false, true).await;
    assert_account_choice_refused(&core, || agent_request(&core, to("p-grok"))).await;
    accepted(core, "p-grok-g2").await;

    let (_fixture, core) = grok_accounts_core("default-original", true, true).await;
    assert_account_choice_refused(&core, || agent_request(&core, to("p-grok-g2"))).await;
    accepted(core, "p-grok").await;

    let (_fixture, core) = grok_accounts_core("default-none", false, false).await;
    accepted(core, "p-grok").await;
}

#[tokio::test]
async fn compaction_profile_pick_cannot_switch_account() {
    let (_fixture, core) = account_choice_core("compaction-pick").await;
    let to = |profile: &str| {
        profile_target_request(
            Some("compaction"),
            vec![("inference_profile_id".into(), Some(json!(profile)))],
        )
        .unwrap()
    };
    assert_account_choice_refused(&core, || to("p-chat-b")).await;
    core.apply(to("p-original")).await.unwrap();
    core.preview(to("p-grok")).await.unwrap();
    core.apply(to("p-grok")).await.unwrap();
}

#[tokio::test]
async fn compaction_reference_pick_cannot_switch_account() {
    let (_fixture, core) = account_choice_core("compaction-ref-pick").await;
    let context = |compaction: &str| {
        protect_working_agent(
            anchored_request(
                SelfConfigTarget::AgentContext,
                "context_id",
                vec![("compaction_id".into(), Some(json!(compaction)))],
            ),
            false,
            true,
        )
    };
    let agent = |context: &str| {
        protect_working_agent(
            agent_request(&core, vec![("context_id".into(), Some(json!(context)))]),
            false,
            true,
        )
    };
    assert_account_choice_refused(&core, || context("c-chat-b")).await;
    assert_account_choice_refused(&core, || agent("ctx-chat-b")).await;
    core.preview(context("c-original")).await.unwrap();
    core.apply(context("c-original")).await.unwrap();
    core.preview(agent("ctx-original")).await.unwrap();
    core.apply(agent("ctx-original")).await.unwrap();
}

#[tokio::test]
async fn agent_management_profile_pick_cannot_switch_account() {
    let (fixture, core) = account_choice_core("agent-pick").await;
    let node = &fixture.db.node;
    let owner = core.node_did().to_owned();
    let params = |action: &str, argv: &[&str]| {
        agent_params(
            action,
            None,
            &argv.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap()
    };
    let create = |profile: &str| {
        params(
            "create",
            &[
                "--display-name",
                "Picker",
                "--description",
                "Picks",
                "--system-prompt",
                "Pick.",
                "--preset",
                "write",
                "--profile",
                profile,
            ],
        )
    };
    let clone = |profile: &str| {
        params(
            "clone",
            &[
                "--from",
                fixture.agent_did(),
                "--display-name",
                "Picker clone",
                "--profile",
                profile,
            ],
        )
    };
    let edit = params(
        "edit",
        &["--id", fixture.agent_did(), "--profile", "p-chat-b"],
    );
    for refused in [create("p-chat-b"), clone("p-chat-b"), edit] {
        let error = model_agent_mutate(&core, fixture.agent.as_ref(), &refused)
            .await
            .expect_err("switching to another account must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
    assert_eq!(crate::list_agents(node, &owner).await.unwrap().len(), 1);
    for accepted in [
        create("p-grok"),
        clone("p-grok"),
        params(
            "edit",
            &["--id", fixture.agent_did(), "--profile", "p-original"],
        ),
        params(
            "edit",
            &["--id", fixture.agent_did(), "--profile", "p-grok"],
        ),
    ] {
        model_agent_mutate(&core, fixture.agent.as_ref(), &accepted)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn agent_preview_agrees_with_the_account_choice_fence() {
    let (fixture, core) = account_choice_core("preview-pick").await;
    let create = [
        "--display-name",
        "Picker",
        "--description",
        "Picks",
        "--system-prompt",
        "Pick.",
        "--preset",
        "write",
    ];
    let clone = [
        "--from",
        fixture.agent_did(),
        "--display-name",
        "Picker clone",
    ];
    let edit = ["--id", fixture.agent_did()];
    for (operation, argv, profile, admitted) in [
        ("create", &create[..], "p-chat-b", false),
        ("clone", &clone[..], "p-chat-b", false),
        ("edit", &edit[..], "p-chat-b", false),
        ("create", &create[..], "p-grok", true),
        ("clone", &clone[..], "p-grok", true),
        ("edit", &edit[..], "p-original", true),
    ] {
        let argv: Vec<String> = argv
            .iter()
            .chain(&["--profile", profile])
            .map(|arg| (*arg).to_owned())
            .collect();
        let params = agent_params("preview", Some(operation.into()), &argv).unwrap();
        let preview: Value =
            serde_json::from_str(&model_agent_preview(&core, &params).await.unwrap()).unwrap();
        assert_eq!(
            preview["admitted"], admitted,
            "{operation} {profile}: {preview}"
        );
        if !admitted {
            assert!(
                preview["rejection"]
                    .as_str()
                    .is_some_and(|rejection| rejection.contains("selects another")),
                "{preview}"
            );
        }
    }
}

#[tokio::test]
async fn agent_clone_inherited_compaction_cannot_switch_account() {
    let (fixture, core) = account_choice_core("clone-pick").await;
    let node = &fixture.db.node;
    let owner = core.node_did().to_owned();
    let original = fixture.install_sibling("src-original").await;
    let chat_b = fixture.install_sibling("src-chat-b").await;
    fixture
        .user_access()
        .transact_write("test.sources", |txn| {
            let sources = [(&original, "ctx-original"), (&chat_b, "ctx-chat-b")];
            let owner = &owner;
            Box::pin(async move {
                let mut documents = Vec::new();
                for (id, context) in sources {
                    let (_, mut value) =
                        crate::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            crate::Collection::Agent,
                            owner,
                            id,
                        )
                        .await?
                        .expect("source Agent exists");
                    value["context_id"] = json!(context);
                    value["inference_profile_id"] = json!("p-original");
                    documents.push(crate::store::config_client::DesiredStateApplyDocument {
                        collection: crate::Collection::Agent,
                        add: value.clone(),
                        update: value,
                    });
                }
                let plan = crate::store::config_client::DesiredStateApplyPlan::new(documents)?;
                crate::store::config_client::apply_desired_state_plan(txn, &plan).await
            })
        })
        .await
        .unwrap();
    let before = crate::list_agents(node, &owner).await.unwrap();
    let clone = |action: &str, source: &str, profile: &str| {
        let name = format!("{source} on {profile}");
        let argv = [
            "--from",
            source,
            "--display-name",
            &name,
            "--profile",
            profile,
        ];
        let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
        let operation = (action == "preview").then(|| "clone".to_owned());
        agent_params(action, operation, &argv).unwrap()
    };
    for (source, profile) in [(chat_b.as_str(), "p-original"), (chat_b.as_str(), "p-grok")] {
        let preview: Value = serde_json::from_str(
            &model_agent_preview(&core, &clone("preview", source, profile))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(preview["admitted"], false, "{source} {profile}: {preview}");
        let error = model_agent_mutate(
            &core,
            fixture.agent.as_ref(),
            &clone("clone", source, profile),
        )
        .await
        .expect_err("inheriting another account's compaction must be refused");
        assert!(
            format!("{error:#}").contains("selects another"),
            "{error:#}"
        );
    }
    assert_eq!(crate::list_agents(node, &owner).await.unwrap(), before);
    for profile in ["p-original", "p-grok"] {
        model_agent_mutate(
            &core,
            fixture.agent.as_ref(),
            &clone("clone", &original, profile),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn explicit_tools_grant_preserves_lsp_settings_guard_for_preview_and_apply() {
    let fixture = SelfConfigFixture::new("self-config-lsp-guard").await;
    fixture.grant_selected_configuration().await;
    let tools = fixture.tools(&["tools"]);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let safe = json!({"integrations":{"lsp":{"config":json!({"servers":{"rust-analyzer":{"disabled":true,"priority":2}}}).to_string()}}});
    config
        .call(json!({"argv":["tools", "preview", "--set", format!("integrations={}", safe["integrations"])]}).to_string())
        .await
        .unwrap();
    config
        .call(json!({"argv":["tools", "edit", "--set", format!("integrations={}", safe["integrations"])]}).to_string())
        .await
        .unwrap();
    let baseline: Value = serde_json::from_str(
        &config
            .call(json!({"argv":["get"]}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        baseline["documents"]["Tools"]["integrations"],
        safe["integrations"]
    );
    for field in ["settings", "init_options", "initOptions"] {
        let server = json!({field:{"check":{"overrideCommand":["custom-check"]}}});
        let raw = json!({"servers":{"rust-analyzer":server}}).to_string();
        crate::tools::native::lsp::LspConfigDocument::parse_operator(Some(&raw)).unwrap();
        let patch = json!({"integrations":{"lsp":{"config":raw}}});
        let preview = config
            .call(json!({"argv":["tools", "preview", "--set", format!("integrations={}", patch["integrations"])]}).to_string())
            .await
            .unwrap_err();
        assert!(preview.to_string().contains(field), "{preview}");
        let apply = config
            .call(json!({"argv":["tools", "edit", "--set", format!("integrations={}", patch["integrations"])]}).to_string())
            .await
            .unwrap_err();
        assert!(apply.to_string().contains(field), "{apply}");
        let after: Value = serde_json::from_str(
            &config
                .call(json!({"argv":["get"]}).to_string())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(after["documents"]["Tools"], baseline["documents"]["Tools"]);
    }
}

#[tokio::test]
async fn profile_edit_rejects_a_context_window_above_the_advertised_maximum() {
    let fixture = SelfConfigFixture::new("profile-context-window").await;
    fixture.grant_selected_configuration().await;
    let owner = fixture.node_did();
    let agent = fixture.agent_did();
    let node = &fixture.db.node;
    let backend_id = format!("{agent}:backend");
    let backend: crate::documents::InferenceBackend = serde_json::from_value(json!({
        "node_did": owner, "backend_id": backend_id, "name": "Test inference",
        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
        "auth": {"kind": "unauthenticated"},
    }))
    .unwrap();
    crate::inference::backends::registry::record_model_catalog(
        node,
        &backend,
        serde_json::from_value(json!({
            "node_did": null,
            "observed_at": "2026-09-25T00:00:00Z",
            "models": [{"model_name":"test-model","context_window":272000,"max_context_window":872000}],
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    let tools = fixture.tools(&["profile"]);
    let edit = |verb: &str, window: u64| -> Vec<String> {
        vec![
            "profile".into(),
            verb.into(),
            "--agent".into(),
            agent.into(),
            "--set".into(),
            format!("context_window={window}"),
        ]
    };

    for verb in ["preview", "edit"] {
        let error = call_config_tool(&tools, edit(verb, 900_000))
            .await
            .expect_err("a window above the advertised maximum must be refused");
        assert!(
            error.contains("advertised maximum 872000"),
            "{verb}: {error}"
        );
    }
    call_config_tool(&tools, edit("edit", 500_000))
        .await
        .expect("a window within the advertised maximum is published");
    let stored = call_config_tool(
        &tools,
        vec![
            "profile".into(),
            "get".into(),
            "--agent".into(),
            agent.into(),
        ],
    )
    .await
    .unwrap();
    assert!(stored.contains("500000"), "{stored}");
}

#[tokio::test]
async fn engineer_configures_targets_executions_and_itself_but_cannot_lock_out() {
    let fixture = SelfConfigFixture::new("engineer-god-mode").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_delete().await;
    fixture.grant_agent_relation_delete().await;
    let lead = fixture.install_sibling("lead").await;
    let caller = fixture.install_sibling("caller").await;
    fixture.grant_sibling_configuration(&lead).await;
    fixture.grant_sibling_configuration(&caller).await;
    let owner = fixture.node_did().to_owned();
    let setup_did = fixture.agent_did().to_owned();
    let setup = fixture.core();
    setup
        .apply(agent_request(
            &setup,
            vec![(
                "tags".into(),
                Some(json!([
                    crate::tools::builtin::self_config::ENGINEER_AGENT_TAG
                ])),
            )],
        ))
        .await
        .unwrap();
    setup
        .apply(tools_request(
            &setup,
            vec![
                (
                    "self_config".into(),
                    Some(json!({"enable_self_config": true, "self_config_no_lockout": true})),
                ),
                ("agents".into(), Some(json!({"enabled": true}))),
            ],
        ))
        .await
        .unwrap();
    let mut grants = config(&["node", "agent", "tools", "profile", "automation"]);
    grants.agent_did = setup_did.clone();
    grants.preview = true;
    grants.no_lockout = true;
    let tools = fixture.tools_with_config(&grants);
    let command = |args: &[&str]| args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let ok = |result: Result<String, String>| -> Value {
        serde_json::from_str(&result.unwrap_or_else(|error| panic!("{error}"))).unwrap()
    };

    let target_patch = [
        "--set",
        "name=\"gatekeeper\"",
        "--set",
        &format!("target_node_did={}", json!(owner)),
        "--set",
        &format!("agent_did={}", json!(lead)),
    ];
    let preview = ok(call_config_tool(
        &tools,
        command(
            &[
                &["agent-target", "preview", "create", "gatekeeper"][..],
                &target_patch,
            ]
            .concat(),
        ),
    )
    .await);
    assert_eq!(preview["committed"], false);
    let missing = call_config_tool(
        &tools,
        command(&[
            "agent-target",
            "preview",
            "create",
            "dangling",
            "--set",
            "name=\"dangling\"",
            "--set",
            &format!("target_node_did={}", json!(owner)),
            "--set",
            "agent_did=\"missing\"",
        ]),
    )
    .await
    .unwrap_err();
    assert!(missing.contains("missing"), "{missing}");
    ok(call_config_tool(
        &tools,
        command(&[&["agent-target", "create", "gatekeeper"][..], &target_patch].concat()),
    )
    .await);
    let listed = ok(call_config_tool(&tools, command(&["agent-target", "list"])).await);
    assert_eq!(listed["items"][0]["target_id"], "gatekeeper");
    ok(call_config_tool(
        &tools,
        command(&[
            "agent-target",
            "edit",
            "gatekeeper",
            "--set",
            "description=\"Reviews lead work\"",
        ]),
    )
    .await);

    for selection in [
        r#"agents={"target_ids":["gatekeeper"]}"#,
        r#"agents={"enabled":false,"target_ids":["gatekeeper"]}"#,
    ] {
        let receipt = ok(call_config_tool(
            &tools,
            command(&["tools", "edit", "--agent", &caller, "--set", selection]),
        )
        .await);
        assert_eq!(receipt["committed"], true);
        assert!(receipt["effect"]
            .as_str()
            .unwrap()
            .contains("Selected targets are inactive"));
        let stored =
            ok(call_config_tool(&tools, command(&["tools", "get", "--agent", &caller])).await);
        assert_ne!(stored["document"]["agents"]["enabled"], true);
        assert_eq!(
            stored["document"]["agents"]["target_ids"],
            json!(["gatekeeper"])
        );
    }

    let recovery = ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--agent",
            &caller,
            "--set",
            r#"agents={"enabled":true,"target_ids":["gatekeeper"]}"#,
        ]),
    )
    .await);
    assert!(!recovery["effect"]
        .as_str()
        .unwrap()
        .contains("Selected targets are inactive"));
    let recovered =
        ok(call_config_tool(&tools, command(&["tools", "get", "--agent", &caller])).await);
    assert_eq!(recovered["document"]["agents"]["enabled"], true);
    assert_eq!(
        recovered["document"]["agents"]["target_ids"],
        json!(["gatekeeper"])
    );

    ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--agent",
            &caller,
            "--set",
            r#"agents={"enabled":true}"#,
        ]),
    )
    .await);

    let enabled = ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--set",
            r#"agents={"enabled":true,"target_ids":["gatekeeper"]}"#,
            "--set",
            r#"built_ins={"enable_session_history_tool":true}"#,
            "--set",
            r#"datastore={"enable_defra_query":true}"#,
        ]),
    )
    .await);
    assert!(!enabled["effect"]
        .as_str()
        .unwrap()
        .contains("Selected targets are inactive"));
    let stored = ok(call_config_tool(&tools, command(&["tools", "get"])).await);
    assert_eq!(stored["document"]["agents"]["enabled"], true);
    for (patch, refusal) in [
        (
            r#"agents={"enabled":false,"target_ids":["gatekeeper"]}"#,
            "agents tools must remain enabled",
        ),
        (
            r#"self_config={"enable_self_config":false}"#,
            "self-config must remain enabled",
        ),
        (
            r#"self_config={"enable_self_config":true,"self_config_no_lockout":false}"#,
            "self_config_no_lockout must remain enabled",
        ),
        (
            r#"self_config={"enable_self_config":true,"self_config_no_lockout":true,"self_config_categories":["profile"]}"#,
            "must keep the tools category",
        ),
    ] {
        for verb in ["preview", "edit"] {
            let error = call_config_tool(&tools, command(&["tools", verb, "--set", patch]))
                .await
                .unwrap_err();
            assert!(error.contains(refusal), "{verb} {patch}: {error}");
        }
    }
    let disable = call_config_tool(
        &tools,
        command(&["agent", "edit", &setup_did, "--set", "enabled=false"]),
    )
    .await
    .unwrap_err();
    assert!(disable.contains("no-lockout"), "{disable}");
    let untag = call_config_tool(
        &tools,
        command(&[
            "agent",
            "edit",
            &setup_did,
            "--set",
            "tags=[\"ui:engineer\"]",
        ]),
    )
    .await
    .unwrap_err();
    assert!(untag.contains("Engineer tag must remain"), "{untag}");
    ok(call_config_tool(
        &tools,
        command(&[
            "agent",
            "edit",
            &setup_did,
            "--set",
            &format!(
                "tags={}",
                json!([
                    crate::tools::builtin::self_config::ENGINEER_AGENT_TAG,
                    "ui:engineer"
                ])
            ),
        ]),
    )
    .await);
    let agent_disable =
        call_config_tool(&tools, command(&["agent", "disable", "--id", &setup_did]))
            .await
            .unwrap_err();
    assert!(agent_disable.contains("no-lockout"), "{agent_disable}");
    for name in ["\"\"", "\"   \""] {
        let blank = call_config_tool(
            &tools,
            command(&[
                "agent-target",
                "edit",
                "gatekeeper",
                "--set",
                &format!("name={name}"),
            ]),
        )
        .await
        .unwrap_err();
        assert!(blank.contains("invalid AgentTarget"), "{blank}");
    }
    ok(call_config_tool(
        &tools,
        command(
            &[
                &["agent-target", "create", "gatekeeper-twin"][..],
                &target_patch,
            ]
            .concat(),
        ),
    )
    .await);
    let duplicate = call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--set",
            r#"agents={"enabled":true,"target_ids":["gatekeeper","gatekeeper-twin"]}"#,
        ]),
    )
    .await
    .unwrap_err();
    assert!(
        duplicate.contains("duplicate agent target name"),
        "{duplicate}"
    );

    for verb in [
        &["execution", "preview", "create"][..],
        &["execution", "create"],
    ] {
        ok(call_config_tool(&tools, command(&[verb, &["default-execution"]].concat())).await);
    }
    let defaults =
        ok(call_config_tool(&tools, command(&["execution", "get", "default-execution"])).await);
    assert_eq!(defaults["document"]["execution_id"], "default-execution");
    assert_eq!(
        defaults["effective"]["max_turns"],
        crate::inference::resolved::DEFAULT_MAX_TURNS
    );
    assert_eq!(
        defaults["effective"]["provider_idle_timeout_secs"],
        crate::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS
    );
    assert_eq!(
        defaults["effective"]["deadline_duration_secs"],
        crate::inference::resolved::DEFAULT_DEADLINE_DURATION_SECS
    );
    assert!(defaults["effective"]["max_total_tokens"].is_null());
    assert!(defaults["document"]["max_turns"].is_null());
    let inventory = ok(call_config_tool(&tools, command(&["execution", "list"])).await);
    let listed = inventory["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["execution_id"] == "default-execution")
        .unwrap();
    assert_eq!(listed["effective"], defaults["effective"]);
    assert!(
        call_config_tool(&tools, command(&["execution", "edit", "default-execution"]))
            .await
            .unwrap_err()
            .contains("empty patch")
    );
    ok(call_config_tool(
        &tools,
        command(&[
            "execution",
            "preview",
            "create",
            "lead-execution",
            "--set",
            "max_turns=40",
        ]),
    )
    .await);
    ok(call_config_tool(
        &tools,
        command(&[
            "execution",
            "create",
            "lead-execution",
            "--set",
            "max_turns=40",
        ]),
    )
    .await);
    ok(call_config_tool(
        &tools,
        command(&[
            "profile",
            "edit",
            "--agent",
            &lead,
            "--set",
            "execution_id=\"lead-execution\"",
        ]),
    )
    .await);
    ok(call_config_tool(
        &tools,
        command(&[
            "profile",
            "edit",
            "execution",
            "--agent",
            &lead,
            "--set",
            "max_turns=12",
        ]),
    )
    .await);
    let execution =
        ok(call_config_tool(&tools, command(&["execution", "get", "lead-execution"])).await);
    assert_eq!(execution["document"]["max_turns"], 12);
    assert_eq!(execution["effective"]["max_turns"], 12);
    let bound = ok(call_config_tool(
        &tools,
        command(&["profile", "get", "execution", "--agent", &lead]),
    )
    .await);
    assert_eq!(bound["effective"], execution["effective"]);

    ok(call_config_tool(
        &tools,
        command(&[
            "automation",
            "edit",
            "task",
            "engineer-inbox",
            "--agent",
            &setup_did,
            "--set",
            "prompt_template=\"Review {{ doc.message }}\"",
        ]),
    )
    .await);

    assert!(call_config_tool(
        &tools,
        command(&["cleanup", "preview", "--target", "agent-target=gatekeeper"]),
    )
    .await
    .is_err());
    let refused = call_config_tool(
        &tools,
        command(&["tools", "edit", "--set", r#"agents={"enabled":true}"#]),
    )
    .await
    .unwrap_err();
    assert!(
        refused.contains("drop existing settings from your own Tools: agents.target_ids")
            && refused.contains("allow-drop"),
        "{refused}"
    );
    ok(call_config_tool(
        &tools,
        command(&[
            "tools",
            "edit",
            "--allow-drop",
            "agents",
            "--set",
            r#"agents={"enabled":true}"#,
        ]),
    )
    .await);
    let cleanup = ok(call_config_tool(
        &tools,
        command(&["cleanup", "preview", "--target", "agent-target=gatekeeper"]),
    )
    .await);
    let digest = cleanup["plan_digest"].as_str().unwrap().to_owned();
    ok(call_config_tool(
        &tools,
        command(&[
            "cleanup",
            "remove",
            "--digest",
            &digest,
            "--target",
            "agent-target=gatekeeper",
        ]),
    )
    .await);
}

#[tokio::test]
async fn backend_reads_expose_operator_catalogs_without_credentials_or_provider_calls() {
    const SECRET: &str = "operator-api-key-never-exposed";
    let fixture = SelfConfigFixture::new("backend-catalog-read").await;
    fixture.grant_node_reader().await;
    fixture.grant_selected_configuration().await;
    let owner = fixture.node_did();
    let access = fixture.user_access();
    let runtime_access =
        crate::ConfigAccess::local_as(fixture.db.node.clone(), identity::Did::new(owner).unwrap());
    let backend = |id: &str, kind: &str, auth: Value| -> crate::documents::InferenceBackend {
        serde_json::from_value(json!({
            "node_did": owner, "backend_id": id, "name": id, "provider_kind": kind,
            "endpoint": "http://127.0.0.1:1/v1", "auth": auth,
        }))
        .unwrap()
    };
    let claude = backend(
        "claude",
        "ClaudeCliSubscription",
        json!({"kind": "node_oauth"}),
    );
    let keyed = backend(
        "keyed",
        "OpenAiCompatible",
        json!({"kind": "api_key", "key": SECRET}),
    );
    let unobserved = backend(
        "unobserved",
        "OpenAiCompatible",
        json!({"kind": "unauthenticated"}),
    );
    let models: Vec<crate::documents::AdvertisedModel> = serde_json::from_value(json!([{
        "model_name": "claude-opus-5-5", "display_name": "Claude Opus 5.5",
        "context_window": 1_000_000, "max_context_window": null, "max_output_tokens": 128_000,
        "reasoning_efforts": ["low", "high"],
    }]))
    .unwrap();
    for document in [&claude, &keyed, &unobserved] {
        crate::store::config_client::write_inference_backend_document(access, document)
            .await
            .unwrap();
    }
    let stored_keyed = access
        .transact_readonly("test.backend_catalog.metadata", |txn| {
            Box::pin(async move {
                crate::store::config_client::load_inference_backend_in_txn(txn, owner, "keyed")
                    .await
            })
        })
        .await
        .unwrap()
        .expect("persisted keyed backend");
    assert!(matches!(
        &stored_keyed.auth,
        crate::documents::BackendAuth::Credential { .. }
    ));
    assert_eq!(
        crate::inference::backends::registry::record_connection_catalog_on(
            &runtime_access,
            owner,
            claude.provider_kind,
            &claude.endpoint,
            &claude.auth,
            models.clone(),
        )
        .await
        .unwrap(),
        1
    );
    crate::inference::backends::registry::record_discovered_catalog_on(
        &runtime_access,
        &stored_keyed,
        models,
    )
    .await
    .unwrap();

    let tools = fixture.tools(&["backend"]);
    let call = |argv: &[&str]| {
        let argv = std::iter::once("backend")
            .chain(argv.iter().copied())
            .map(String::from)
            .collect::<Vec<_>>();
        let tools = &tools;
        async move {
            let output = call_config_tool(tools, argv).await.unwrap();
            assert!(!output.contains(SECRET), "credential exposed: {output}");
            serde_json::from_str::<Value>(&output).unwrap()
        }
    };

    let list = call(&["list"]).await;
    let items = list["items"].as_array().unwrap();
    let page = call(&["list", "--limit", "1"]).await;
    assert_eq!(page["items"][0], items[0]);
    assert_eq!(page["page"]["total"], items.len());
    assert_eq!(page["page"]["truncated"], true);
    for id in ["claude", "keyed"] {
        let item = items.iter().find(|item| item["backend_id"] == id).unwrap();
        assert_eq!(
            item["catalog"]["models"][0]["model_name"],
            "claude-opus-5-5"
        );
        assert!(item.get("catalogs").is_none() && item.get("auth").is_none());
    }
    for id in ["claude", "keyed"] {
        let get = call(&["get", id]).await;
        let listed = items.iter().find(|item| item["backend_id"] == id).unwrap();
        for field in ["catalog", "probe_status", "last_probe"] {
            assert_eq!(listed[field], get["observation"][field], "{id}: {field}");
        }
        let model = &get["observation"]["catalog"]["models"][0];
        assert_eq!(model["context_window"], 1_000_000);
        assert_eq!(model["reasoning_efforts"], json!(["low", "high"]));
        let discover = call(&["discover", id]).await;
        assert_eq!(discover["refreshed"], false);
        assert_eq!(
            discover["observation"]["catalog"]["models"][0]["model_name"],
            "claude-opus-5-5"
        );
    }
    let item = items
        .iter()
        .find(|item| item["backend_id"] == "unobserved")
        .unwrap();
    assert!(item["catalog"].is_null());
    assert!(item["probe_status"].is_null());
    assert!(item["last_probe"].is_null());
    let bound = call(&["get"]).await;
    assert!(bound["observation"].get("catalog").is_some(), "{bound}");
}

#[tokio::test]
async fn modeled_resource_reads_use_resource_category_and_native_access() {
    let fixture = SelfConfigFixture::new("resource-read-exposure").await;
    fixture.grant_node_reader().await;
    fixture.grant_selected_configuration().await;
    let backend_id = format!("{}:backend", fixture.agent_did());
    let task = json!({"node_did": fixture.node_did(), "task_id": "read-exposure-task",
        "agent_did": fixture.agent_did(), "prompt_template": "Do the work"});
    let task_plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::Task,
            add: task.clone(),
            update: task,
        },
    ])
    .unwrap();
    fixture
        .user_access()
        .transact_write("test.read_exposure.task", |txn| {
            let task_plan = &task_plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, task_plan).await
            })
        })
        .await
        .unwrap();
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().self_config_resource_read_exposure_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let categories = case["categories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        let expected = case["exposed"].as_bool().unwrap();
        let tools = fixture.tools(&categories);
        let commands = match case["resource"].as_str().unwrap() {
            "backend" => vec![
                vec!["backend".to_owned(), "list".to_owned()],
                vec!["backend".to_owned(), "get".to_owned(), backend_id.clone()],
                vec!["backend".to_owned(), "get".to_owned()],
            ],
            "tools" => vec![vec!["tools".into(), "get".into()]],
            "profile" => vec![
                vec!["profile".into(), "list".into()],
                vec!["profile".into(), "get".into()],
            ],
            "automation" => vec![
                vec!["automation".into(), "list".into(), "task".into()],
                vec![
                    "automation".into(),
                    "get".into(),
                    "task".into(),
                    "read-exposure-task".into(),
                ],
            ],
            "effective" => vec![
                vec!["get".into()],
                vec![
                    "get".into(),
                    "--agent".into(),
                    fixture.agent_did().to_owned(),
                ],
            ],
            other => panic!("unexpected modeled read resource {other}"),
        };
        for command in commands {
            let result = call_config_tool(&tools, command.clone()).await;
            assert_eq!(result.is_ok(), expected, "{case}: {command:?}: {result:?}");
            if expected && case["resource"] == "effective" {
                let value: Value = serde_json::from_str(result.as_ref().unwrap()).unwrap();
                assert_eq!(value["agent_did"], fixture.agent_did());
            }
            if expected
                && command.first().is_some_and(|part| part == "automation")
                && command.get(1).is_some_and(|part| part == "get")
            {
                let value: Value = serde_json::from_str(result.as_ref().unwrap()).unwrap();
                assert_eq!(value["document"]["agent_did"], fixture.agent_did());
                assert_eq!(value["document"]["task_id"], "read-exposure-task");
            }
        }
    }
}

#[tokio::test]
async fn automation_read_preserves_native_selected_agent_visibility() {
    let fixture = SelfConfigFixture::new("automation-read-native-scope").await;
    fixture.grant_selected_configuration().await;
    let sibling = fixture.install_sibling("Other Agent").await;
    let task = json!({"node_did": fixture.node_did(), "task_id": "other-agent-task",
        "agent_did": sibling, "prompt_template": "Do the work"});
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection: crate::Collection::Task,
            add: task.clone(),
            update: task,
        },
    ])
    .unwrap();
    fixture
        .user_access()
        .transact_write("test.automation_read.other_task", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
    let expected = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap()
        .configuration_task_target_cases
        .iter()
        .find(|case| case["name"] == "task-unrelated")
        .unwrap()["expected"]
        .as_bool()
        .unwrap();
    let access = crate::ConfigAccess::local_managed_agent(
        fixture.db.node.clone(),
        identity::Did::new(fixture.agent_did()).unwrap(),
        fixture.agent.as_ref(),
    )
    .unwrap();
    let response = access
        .execute("{ Task(filter: {task_id: {_eq: \"other-agent-task\"}}) {_docID} }")
        .await
        .unwrap();
    let visible = !response["data"]["Task"].as_array().unwrap().is_empty();
    assert_eq!(visible, expected, "native selected Task access");
    let tools = fixture.tools(&["automation"]);
    let result = call_config_tool(
        &tools,
        vec![
            "automation".into(),
            "get".into(),
            "task".into(),
            "other-agent-task".into(),
            "--agent".into(),
            sibling,
        ],
    )
    .await;
    assert_eq!(
        result.is_ok(),
        expected,
        "tool must retain invoking Agent access: {result:?}"
    );
}

#[tokio::test]
async fn pack_search_uses_registry_pagination_without_installing() {
    let _registry_lock = PACK_REGISTRY_ENV.lock().await;
    let app = axum::Router::new().route("/api/v1/packs", axum::routing::get(
        |axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>| async move {
            assert_eq!(query.get("q").map(String::as_str), Some("code review"));
            let page = &query["page"];
            axum::Json(json!({"packs":[{"namespace":"fixture","name":"documents_fixture","latest":"1.0.0","description":"Code review"}],"has_more":page == "1"}))
        }
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let _registry =
        crate::test_support::EnvVarGuard::set("GENTS_REGISTRY", format!("http://{address}"));
    let (fixture, tools) = signed_pack_tool("registry-search", test_plugins()).await;
    let tool = tools.iter().find(|t| t.name() == CONFIG_TOOL_NAME).unwrap();
    let first: Value = serde_json::from_str(
        &tool
            .call(json!({"argv":["pack","search"],"options":{"query":"code review"}}).to_string())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        first["items"][0]["inspect"],
        json!({"argv":["pack","get","fixture/documents_fixture"]})
    );
    let next: Value =
        serde_json::from_str(&tool.call(first["next_call"].to_string()).await.unwrap()).unwrap();
    assert_eq!(next["page"], 2);
    assert!(next["next_call"].is_null());
    for options in [
        json!({"page":0}),
        json!({"page":"abc"}),
        json!({"registry":"http://untrusted"}),
    ] {
        assert!(tool
            .call(json!({"argv":["pack","search"],"options":options}).to_string())
            .await
            .is_err());
    }
    assert!(crate::pack::read_installed_pack(
        &fixture.user_access,
        fixture.db.node_identity.did(),
        "fixture/documents_fixture",
    )
    .await
    .unwrap()
    .is_none());
    server.abort();
}

#[tokio::test]
async fn pack_documents_install_and_release_documents_dependencies() {
    let (home, plugins) = crate::test_support::home_with_fixture_pack("dependent_fixture");
    let (_fixture, directory) =
        crate::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    let (bytes, _) = crate::pack::archive::pack_dir(&directory).unwrap();
    crate::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap();
    let (fixture, tools) = signed_pack_tool("documents-dependencies", plugins).await;
    let did = fixture.db.node_identity.did();
    let slot = format!("worker={}:inference", fixture.agent_did());
    let slots = ["--inference-slot", slot.as_str()];
    let mut args = vec!["pack", "preview", "install", "fixture/dependent_fixture"];
    args.extend(slots);
    let preview: Value = serde_json::from_str(&config_call(&tools, &args).await.unwrap()).unwrap();
    assert_eq!(preview["ready"], true, "{preview}");
    assert_eq!(
        preview["dependencies"][0]["coordinate"],
        "fixture/dependee_fixture"
    );
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut args = vec![
        "pack",
        "install",
        "fixture/dependent_fixture",
        "--digest",
        digest,
    ];
    args.extend(slots);
    config_call(&tools, &args).await.unwrap();
    let access = &fixture.user_access;
    let rows = access
        .execute(
            r#"{ PackInstallation(filter: {coordinate: {_eq: "fixture/dependee_fixture"}}) { explicit required_by } }"#,
        )
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["PackInstallation"],
        json!([{"explicit": false, "required_by": ["fixture/dependent_fixture"]}])
    );
    config_call(&tools, &["pack", "remove", "fixture/dependent_fixture"])
        .await
        .unwrap();
    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/dependee_fixture")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn pack_install_blocks_a_dependency_plugin_without_operator_authority() {
    let edit = |dir: &std::path::Path, change: &dyn Fn(&mut Value)| {
        let path = dir.join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        change(&mut manifest);
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    };
    let (_dependee, dependee_dir) =
        crate::test_support::fixture_pack_copy("dependee_fixture", &json!({}));
    std::fs::create_dir_all(dependee_dir.join("plugins")).unwrap();
    let wat = crate::plugin::tests::constant_output_wat(b"{}");
    std::fs::write(
        dependee_dir.join("plugins/echo.afb"),
        crate::plugin::tests::build_plugin_afb(&wat),
    )
    .unwrap();
    edit(&dependee_dir, &|manifest| {
        manifest["assets"]
            .as_array_mut()
            .unwrap()
            .push(json!("plugins/echo.afb"));
        manifest["plugins"] = json!([{
            "name": "echo",
            "description": "Echoes nothing.",
            "artifact": "plugins/echo.afb",
            "language": "rust",
            "input_schema": {"type": "object"},
            "manifold": {
                "fs": "None",
                "net": {"OutboundHttp": ["api.example.com"]},
                "env": "None",
                "crypto": false,
                "child_process": false,
            },
        }]);
    });
    let (_dependent, dependent_dir) =
        crate::test_support::fixture_pack_copy("dependent_fixture", &json!({}));
    let (home, plugins) = crate::test_support::home_with_pack_dir(&dependent_dir);
    let (bytes, _) = crate::pack::archive::pack_dir(&dependee_dir).unwrap();
    crate::pack::store::PackStore::new(home.path())
        .import(&bytes[..], None)
        .unwrap();
    let (fixture, tools) = signed_pack_tool("dependency-plugin-authority", plugins).await;
    let slot = format!("worker={}:inference", fixture.agent_did());
    let slots = ["--inference-slot", slot.as_str()];
    let mut preview_argv = vec!["pack", "preview", "install", "fixture/dependent_fixture"];
    preview_argv.extend(slots);
    let preview: Value =
        serde_json::from_str(&config_call(&tools, &preview_argv).await.unwrap()).unwrap();
    assert_eq!(preview["ready"], false, "{preview}");
    assert!(preview["installation_blockers"]
        .to_string()
        .contains("api.example.com"));
    let digest = preview["artifact_digest"].as_str().unwrap();
    let mut install_argv = vec![
        "pack",
        "install",
        "fixture/dependent_fixture",
        "--digest",
        digest,
    ];
    install_argv.extend(slots);
    let error = format!(
        "{:#}",
        config_call(&tools, &install_argv).await.unwrap_err()
    );
    assert!(error.contains("api.example.com"), "{error}");
    assert!(
        crate::plugin::store::read_record(home.path(), "fixture", "echo").is_err(),
        "a refused plugin leaves no record"
    );
}

#[tokio::test]
async fn pack_document_installation_uses_existing_lifecycle() {
    let (_fixture, directory) =
        crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let manifest_path = directory.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (_home, plugins) = crate::test_support::home_with_pack_dir(&directory);
    let (fixture, tools) = signed_pack_tool("inspect-documents-pack", plugins).await;
    let did = fixture.db.node_identity.did();
    let slot = format!("worker={}:inference", fixture.agent_did());
    let result: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", "fixture/documents_fixture"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(result["installable"], true);
    let missing: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "preview", "install", "fixture/documents_fixture"],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(missing["ready"], false);
    assert_eq!(missing["missing_inference_slots"], json!(["worker"]));
    let preview: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                "fixture/documents_fixture",
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], true, "{preview}");
    let digest = preview["artifact_digest"].as_str().unwrap();
    let access = &fixture.user_access;
    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none(),
        "preview must not install documents"
    );
    let rejected = config_call(
        &tools,
        &[
            "pack",
            "install",
            "fixture/documents_fixture",
            "--digest",
            "sha256:wrong",
            "--inference-slot",
            &slot,
        ],
    )
    .await
    .unwrap_err();
    assert!(rejected.to_string().contains("digest"), "{rejected}");
    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "install",
                "fixture/documents_fixture",
                "--digest",
                digest,
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["effective"]["digest"], digest);
    assert!(
        fixture
            .db
            .node
            .get_collection("FixtureJob")
            .unwrap()
            .is_some(),
        "custom schema asset must be published"
    );

    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_some()
    );
    let updated: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "update",
                "fixture/documents_fixture@1.0.0",
                "--digest",
                digest,
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["effective"]["digest"], digest);
    config_call(&tools, &["pack", "remove", "fixture/documents_fixture"])
        .await
        .unwrap();
    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
}

async fn account_state_rows(fixture: &SelfConfigFixture) -> Value {
    let access = crate::inference::auth::credential::runtime_credential_access(
        &fixture.db.node,
        fixture.node_did(),
    )
    .unwrap();
    let credentials = access
        .execute("{ OAuthCredential { _docID credential_id enabled label access_token } }")
        .await
        .unwrap();
    let backend = fixture.user_access.execute("{ InferenceBackendMetadata { _docID backend_id enabled auth_kind } BackendCredential { _docID credential_id secret } }").await.unwrap();
    json!({"credentials": credentials["data"], "backend": backend["data"]})
}

async fn seed_account_view(
    cache: &crate::inference::usage_observation::UsageCache,
    fixture: &SelfConfigFixture,
) {
    use crate::inference::auth::credential::{
        oauth_credential_id, upsert_oauth_credential, OAuthCredential,
    };
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let provider = crate::inference::auth::claude::CLAUDE_OAUTH_PROVIDER;
    let account = |account_ref: Option<&str>, label: &str, enabled: bool| OAuthCredential {
        doc_id: None,
        credential_id: match account_ref {
            Some(account_ref) => format!("{}:{account_ref}", oauth_credential_id(owner, provider)),
            None => oauth_credential_id(owner, provider),
        },
        node_did: owner.to_string(),
        provider: provider.to_string(),
        access_token: "access-SECRET".into(),
        refresh_token: "refresh-SECRET".into(),
        id_token: None,
        account_id: Some("identity-SECRET".into()),
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled,
        account_ref: account_ref.map(str::to_string),
        connected_at: account_ref.map(|_| chrono::Utc::now()),
        provider_account_key: None,
        label: Some(label.to_string()),
    };
    upsert_oauth_credential(node, &account(None, "Agentl", true))
        .await
        .unwrap();
    upsert_oauth_credential(node, &account(Some("acct-l2"), "Work", false))
        .await
        .unwrap();
    let access = fixture.user_access();
    for (backend_id, kind, auth, endpoint) in [
        (
            "backend-usage-claude",
            "ClaudeCliSubscription",
            json!({ "kind": "node_oauth" }),
            "claude-cli://subscription",
        ),
        (
            "backend-usage-work",
            "ClaudeCliSubscription",
            json!({ "kind": "node_oauth", "account_ref": "acct-l2" }),
            "claude-cli://subscription",
        ),
        (
            "backend-usage-key",
            "OpenRouter",
            json!({ "kind": "api_key", "key": "key-SECRET" }),
            "http://127.0.0.1:9/api/v1",
        ),
    ] {
        let backend: crate::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner,
            "backend_id": backend_id,
            "name": backend_id,
            "provider_kind": kind,
            "endpoint": endpoint,
            "auth": auth,
        }))
        .unwrap();
        crate::store::config_client::write_inference_backend_document(access, &backend)
            .await
            .unwrap();
    }
    let profile = serde_json::from_value(json!({
        "node_did": owner,
        "profile_id": "profile-usage-a",
        "backend_id": "backend-usage-claude",
        "model_name": "model-x",
    }))
    .unwrap();
    crate::store::config_client::write_inference_profile_document(access, &profile)
        .await
        .unwrap();
    crate::inference::usage_observation::record_usage(
        cache,
        node,
        &crate::inference::usage_observation::UsageAccount::Credential {
            doc_id: None,
            node_did: owner.to_string(),
            provider: provider.to_string(),
            account_ref: None,
        },
        crate::inference::usage_observation::account_usage::UsageReport {
            windows: vec![
                crate::inference::usage_observation::account_usage::UsageWindow {
                    label: "5h".into(),
                    window_minutes: Some(300),
                    used_pct: 42.0,
                    resets_at: None,
                    source: crate::inference::usage_observation::account_usage::UsageSource::Header,
                    observed_at: chrono::Utc::now(),
                },
            ],
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn config_backend_accounts_lists_accounts_and_usage_read_only() {
    let fixture = SelfConfigFixture::new("config-accounts").await;
    fixture.grant_node_reader().await;
    let cache = std::sync::Arc::new(
        crate::inference::usage_observation::UsageCache::with_endpoints(
            crate::inference::usage_observation::UsageEndpoints {
                claude_usage: "http://127.0.0.1:9/usage".into(),
                grok_billing: "http://127.0.0.1:9/billing".into(),
            },
        ),
    );
    seed_account_view(&cache, &fixture).await;
    let mut tool_config = config(&["node", "profile", "backend"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    let tools = build_self_config_tools_with_runtime(
        cache,
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        Some(fixture.agent.clone()),
        Some(fixture.db.node_identity.clone()),
        &tool_config,
        test_plugins(),
        Some(fixture.node_binding.clone()),
    );
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let before = account_state_rows(&fixture).await;

    let text = config
        .call(json!({"argv":["backend", "accounts"]}).to_string())
        .await
        .unwrap();

    let view: Value = serde_json::from_str(&text).unwrap();
    let items = view["items"].as_array().expect("items");
    let item = |label: &str| {
        items
            .iter()
            .find(|item| item["label"] == label)
            .unwrap_or_else(|| panic!("no {label}: {text}"))
    };
    let agentl = item("Agentl");
    assert_eq!(agentl["provider"], "claude-subscription");
    assert_eq!(agentl["state"], "enabled");
    assert_eq!(agentl["profiles"], json!(["profile-usage-a"]));
    assert_eq!(agentl["usage"]["windows"][0]["used_pct"], 42.0);
    let work = item("Work");
    assert_eq!(work["state"], "disabled");
    assert_eq!(work["usage"], Value::Null);
    let key = item("backend-usage-key");
    assert_eq!(key["provider"], "OpenRouter");
    assert_eq!(key["state"], "enabled");
    assert_eq!(key["usage"]["note"], "unknown");
    assert!(
        view["note"].as_str().unwrap().contains("Read-only"),
        "{text}"
    );
    for hidden in [
        "SECRET",
        "identity-",
        "did:key",
        "credential_id",
        "account_ref",
        "acct-l2",
    ] {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    assert_eq!(account_state_rows(&fixture).await, before);

    let help = config
        .call(json!({"argv":["help", "backend"]}).to_string())
        .await
        .unwrap();
    assert!(help.contains("backend accounts"), "{help}");
    assert!(config
        .call(json!({"argv":["backend", "accounts", "--set", "x"]}).to_string())
        .await
        .is_err());
}

#[tokio::test]
async fn config_backend_accounts_is_readable_without_the_backend_grant() {
    let fixture = SelfConfigFixture::new("config-accounts-grant").await;
    let tools = fixture.tools(&["node", "profile"]);
    let config = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    config
        .call(json!({"argv":["backend", "accounts"]}).to_string())
        .await
        .unwrap();
    assert!(config
        .call(json!({"argv":["backend", "list"]}).to_string())
        .await
        .is_err());
}

#[tokio::test]
async fn agent_management_clears_optional_fields_and_refuses_ignored_tool_modifiers() {
    let fixture = SelfConfigFixture::new("agent-clear").await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let access = fixture.user_access();
    let user = identity::Did::new(access.actor_did().unwrap()).unwrap();
    let binding = crate::store::identity::provision_agent_identity_authorized(
        node,
        fixture.db.node_identity.as_ref(),
        &user,
    )
    .await
    .unwrap();
    let profile = format!("{}:inference", fixture.agent_did());
    let created: Value = serde_json::from_str(
        &configure_agent(
            access,
            owner,
            &ConfigureAgentParams {
                action: "create".into(),
                identity_binding: Some(binding),
                display_name: StringUpdate::Set("Clearable".into()),
                description: StringUpdate::Set("Original description".into()),
                system_prompt: StringUpdate::Set("Original instructions".into()),
                profile_id: StringUpdate::Set(profile.clone()),
                ..Default::default()
            },
            &Default::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let id = created["agent_did"].as_str().unwrap().to_owned();
    let edit = ConfigureAgentParams {
        action: "edit".into(),
        agent_did: Some(id.clone()),
        display_name: StringUpdate::Clear,
        description: StringUpdate::Clear,
        system_prompt: StringUpdate::Clear,
        ..Default::default()
    };
    configure_agent(access, owner, &edit, &Default::default())
        .await
        .unwrap();
    let stored = crate::list_agents(node, owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == id)
        .unwrap();
    assert_eq!(stored.display_name, None);
    assert_eq!(stored.description, None);
    let context_id = crate::store::graphql::escape_graphql_string(
        stored
            .context_id
            .as_deref()
            .expect("created agent has a Context"),
    );
    let context = access.execute(&format!(
        r#"{{ AgentContext(filter: {{ context_id: {{ _eq: "{context_id}" }} }}) {{ system_prompt }} }}"#,
    )).await.unwrap();
    assert!(context["data"]["AgentContext"][0]["system_prompt"].is_null());
    assert_eq!(stored.inference_profile_id, profile);
    let invalid = ConfigureAgentParams {
        root: StringUpdate::Set("/ignored".into()),
        ..edit
    };
    let error = configure_agent(access, owner, &invalid, &Default::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("tools update"), "{error:#}");
    let after = crate::list_agents(node, owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == id)
        .unwrap();
    assert_eq!(stored, after);
}

#[tokio::test]
async fn no_lockout_rejects_current_agent_disable_in_preview_and_commit() {
    let fixture = SelfConfigFixture::new("disable-preview-parity").await;
    fixture.grant_selected_configuration().await;
    let default = fixture.install_sibling("default").await;
    fixture.grant_node_reader().await;
    fixture
        .update_signed_configuration(
            crate::Collection::Node,
            fixture.node_did(),
            json!({"default_agent_did": default}),
        )
        .await;
    let current = fixture.agent_did();
    let mut grants = config(&["node"]);
    grants.agent_did = current.to_owned();
    grants.no_lockout = true;
    grants.preview = true;
    let tools = fixture.tools_with_config(&grants);
    for argv in [
        vec!["agent", "preview", "disable", "--id", current],
        vec!["agent", "disable", "--id", current],
    ] {
        let error = call_config_tool(&tools, argv.into_iter().map(str::to_owned).collect())
            .await
            .unwrap_err();
        assert!(
            error.contains("no-lockout guard: agent must remain enabled"),
            "{error}"
        );
    }
    let current = crate::list_agents(&fixture.db.node, fixture.node_did())
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == current)
        .unwrap();
    assert!(current.enabled);
}

#[tokio::test]
async fn agent_only_edits_preserve_an_optional_absent_context() {
    let fixture = SelfConfigFixture::new("contextless-agent-edit").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_agent_relation_delete().await;
    let other = fixture.install_sibling("other").await;
    fixture.grant_node_reader().await;
    fixture.grant_sibling_configuration(&other).await;
    let current = fixture.agent_did();
    let other_profile = format!("{other}:inference");
    let core = fixture.core();
    core.apply(agent_request(&core, vec![("context_id".into(), None)]))
        .await
        .unwrap();
    let tools = fixture.tools(&["agent"]);
    for (prefix, committed) in [
        (vec!["agent", "preview", "edit", current], false),
        (vec!["agent", "edit", current], true),
    ] {
        let mut argv: Vec<String> = prefix.into_iter().map(str::to_owned).collect();
        argv.extend(
            [
                "--set",
                r#"display_name="Contextless""#,
                "--set",
                &format!(
                    "inference_profile_id={}",
                    serde_json::to_string(&other_profile).unwrap()
                ),
            ]
            .into_iter()
            .map(str::to_owned),
        );
        let result: Value =
            serde_json::from_str(&call_config_tool(&tools, argv).await.unwrap()).unwrap();
        assert_eq!(result["committed"], committed);
    }
    let current = crate::list_agents(&fixture.db.node, fixture.node_did())
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == current)
        .unwrap();
    assert_eq!(current.display_name.as_deref(), Some("Contextless"));
    assert_eq!(current.inference_profile_id, other_profile);
    assert_eq!(current.context_id, None);
}

#[tokio::test]
async fn public_agent_management_uses_provisioned_binding_and_client_access() {
    let fixture = SelfConfigFixture::new("public-agent-client").await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let access = fixture.user_access();
    let caller = identity::Did::new(access.actor_did().unwrap()).unwrap();
    let binding = crate::store::identity::provision_agent_identity_authorized(
        node,
        fixture.db.node_identity.as_ref(),
        &caller,
    )
    .await
    .unwrap();
    let mut args = ConfigureAgentParams {
        action: "preview".into(),
        operation: Some("create".into()),
        identity_binding: Some(binding.clone()),
        display_name: StringUpdate::Set("Public agent".into()),
        system_prompt: StringUpdate::Set("Complete the requested work.".into()),
        profile_id: StringUpdate::Set(format!("{}:inference", fixture.agent_did())),
        ..Default::default()
    };
    let preview: Value = serde_json::from_str(
        &configure_agent(access, owner, &args, &Default::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["admitted"], true);
    assert_eq!(preview["committed"], false);
    assert_eq!(crate::list_agents(node, owner).await.unwrap().len(), 1);
    args.action = "create".into();
    args.operation = None;
    args.identity_binding = None;
    let error = configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("runtime-provisioned identity binding"));
    args.identity_binding = Some(binding.clone());
    let committed: Value = serde_json::from_str(
        &configure_agent(access, owner, &args, &Default::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["agent_did"], binding.agent_did);
    assert_eq!(committed["agent_did"], preview["agent_did"]);
    let stored = crate::load_agent(node, &binding.agent_did)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.identity_binding(), binding);
    let signer = crate::store::identity::resolve_agent_identity(
        fixture.db.node_identity.as_ref(),
        &stored.identity_binding(),
    )
    .unwrap();
    assert_eq!(signer.did(), stored.agent_did);
}

#[tokio::test]
async fn public_agent_edit_preserves_disabled_state_and_rejects_clone_options() {
    let fixture = SelfConfigFixture::new("disabled-agent-edit").await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let target = fixture.install_sibling("target").await;
    let access = fixture.user_access();
    let mut stored = crate::load_agent(node, &target).await.unwrap().unwrap();
    stored.enabled = false;
    crate::store::config_client::write_agent_document(access, &stored)
        .await
        .unwrap();
    let mut args = ConfigureAgentParams {
        action: "preview".into(),
        operation: Some("edit".into()),
        agent_did: Some(target.clone()),
        display_name: StringUpdate::Set("Edited while disabled".into()),
        ..Default::default()
    };
    let preview: Value = serde_json::from_str(
        &configure_agent(access, owner, &args, &Default::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["agent"]["enabled"], false);
    assert_eq!(
        crate::load_agent(node, &target).await.unwrap(),
        Some(stored.clone())
    );
    args.action = "edit".into();
    args.operation = None;
    args.clone_from = Some(target.clone());
    let error = configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not accept clone_from"));
    assert_eq!(
        crate::load_agent(node, &target).await.unwrap(),
        Some(stored.clone())
    );
    args.clone_from = None;
    configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap();
    stored.display_name = Some("Edited while disabled".into());
    assert_eq!(
        crate::load_agent(node, &target).await.unwrap(),
        Some(stored.clone())
    );
    args.profile_id = StringUpdate::Set("missing-profile".into());
    assert!(configure_agent(access, owner, &args, &Default::default())
        .await
        .is_err());
    assert_eq!(
        crate::load_agent(node, &target).await.unwrap(),
        Some(stored.clone())
    );
    args.profile_id = StringUpdate::Omitted;
    args.make_default = true;
    assert!(configure_agent(access, owner, &args, &Default::default())
        .await
        .is_err());
    assert_eq!(
        crate::load_agent(node, &target).await.unwrap(),
        Some(stored)
    );
}

#[tokio::test]
async fn public_prompt_edits_require_an_existing_unshared_context_without_partial_writes() {
    let fixture = SelfConfigFixture::new("prompt-context-ownership").await;
    fixture.grant_user_relation_delete().await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let missing_id = fixture.install_sibling("missing").await;
    let shared_id = fixture.install_sibling("shared").await;
    let sibling_id = fixture.install_sibling("sibling").await;
    let private_id = fixture.install_sibling("private").await;
    let access = fixture.user_access();
    let mut missing = crate::load_agent(node, &missing_id).await.unwrap().unwrap();
    missing.context_id = None;
    crate::store::config_client::write_agent_document(access, &missing)
        .await
        .unwrap();
    let shared = crate::load_agent(node, &shared_id).await.unwrap().unwrap();
    let mut sibling = crate::load_agent(node, &sibling_id).await.unwrap().unwrap();
    sibling.context_id = shared.context_id;
    crate::store::config_client::write_agent_document(access, &sibling)
        .await
        .unwrap();
    let query = "query { Agent(order: {agent_did: ASC}) { agent_did display_name context_id inference_profile_id enabled } AgentContext(order: {context_id: ASC}) { context_id display_name system_prompt tools_id } }";
    let before = access.execute(query).await.unwrap();
    for (target, expected) in [
        (missing_id.as_str(), "agent has no context"),
        (shared_id.as_str(), "unshared context"),
    ] {
        for preview in [true, false] {
            let args = ConfigureAgentParams {
                action: if preview { "preview" } else { "edit" }.into(),
                operation: preview.then(|| "edit".into()),
                agent_did: Some(target.into()),
                display_name: StringUpdate::Set("Must not be published".into()),
                system_prompt: StringUpdate::Set("Replacement instructions".into()),
                ..Default::default()
            };
            let error = configure_agent(access, owner, &args, &Default::default())
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(access.execute(query).await.unwrap(), before);
        }
    }
    let private = crate::load_agent(node, &private_id).await.unwrap().unwrap();
    let args = ConfigureAgentParams {
        action: "edit".into(),
        agent_did: Some(private_id),
        system_prompt: StringUpdate::Set("Private replacement instructions".into()),
        ..Default::default()
    };
    configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap();
    let after = access.execute(query).await.unwrap();
    assert_eq!(after["data"]["Agent"], before["data"]["Agent"]);
    let mut expected_contexts = before["data"]["AgentContext"].clone();
    let changed = expected_contexts
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|context| context["context_id"].as_str() == private.context_id.as_deref())
        .unwrap();
    changed["system_prompt"] = json!("Private replacement instructions");
    assert_eq!(after["data"]["AgentContext"], expected_contexts);
}

#[tokio::test]
async fn modeled_job_policy_updates_follow_native_tools_authorization() {
    let cases = &crate::lean_vocab_test::lean_self_config_selection_cases().job_guard_cases;
    let native_cases: Vec<_> = cases
        .iter()
        .filter(|case| case.name.starts_with("native_agent_update_"))
        .collect();
    assert!(native_cases
        .iter()
        .any(|case| case.native_authorized && case.native_source_authorized));
    assert!(native_cases.iter().any(|case| !case.native_authorized));
    assert!(native_cases.iter().any(|case| {
        case.native_authorized && !case.native_source_authorized && case.add_schema_tool
    }));
    for case in native_cases {
        let fixture = SelfConfigFixture::new(&case.name).await;
        let tools_id = format!("{}:tools", fixture.agent_did());
        let node_did = fixture.node_did().to_owned();
        fixture
            .update_signed_configuration(
                crate::Collection::Tools,
                &tools_id,
                json!({"integrations": case.stored_integrations}),
            )
            .await;
        if case.native_authorized {
            fixture.grant_selected_configuration().await;
        }
        let (doc_id, before) = fixture
            .user_access()
            .transact_readonly("test.job_policy.tools_before", |txn| {
                let tools_id = &tools_id;
                let node_did = &node_did;
                Box::pin(async move {
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Tools,
                        node_did,
                        tools_id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("selected Tools exists");
        let actor = identity::Did::new(fixture.agent_did()).unwrap();
        let decisions = crate::ConfigAccess::local_as(
            fixture.db.node.clone(),
            identity::Did::new(&node_did).unwrap(),
        )
        .transact_readonly("test.job_policy.native_update", |txn| {
                let actor = actor.clone();
                let doc_id = doc_id.clone();
                Box::pin(async move {
                    txn.observe_document_access(
                        &actor,
                        "Tools",
                        &[doc_id],
                        defra_node::DocumentAccessMode::Named {
                            permission: "update".into(),
                            node_permission:
                                crate::store::config_client::NodePermission::DocumentUpdate,
                        },
                    )
                    .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            decisions == [defra_node::DocumentAccessDecision::Allowed],
            case.native_authorized,
            "{}: native Tools update",
            case.name
        );
        assert!(!fixture
            .db
            .node
            .nac_manager()
            .is_admin(&actor)
            .await
            .unwrap());
        let mut patch = vec![(
            "integrations".into(),
            Some(case.candidate_integrations.clone()),
        )];
        if case.add_schema_tool {
            let mut built_ins = before
                .get("built_ins")
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            assert_ne!(built_ins["enable_schema_tool"], true);
            built_ins["enable_schema_tool"] = json!(true);
            patch.push(("built_ins".into(), Some(built_ins)));
        }
        let core = fixture.core();
        let result = core.apply(tools_request(&core, patch)).await;
        assert_eq!(result.is_ok(), case.accepted, "{}: {result:?}", case.name);
        if let Err(error) = result {
            if !case.native_authorized {
                let denied = gents_protocol::error_chain::find_map_source(
                    error.as_ref(),
                    |cause| {
                        cause
                    .downcast_ref::<crate::store::config_client::ConfigClientError>()
                    .map(|error| {
                        error.is_permission_denied()
                            || error.is_missing_or_denied_write()
                            || matches!(error, crate::store::config_client::ConfigClientError::ConfigurationMutationNotApplied { .. })
                    })
                    },
                );
                assert_eq!(denied, Some(true), "{}: {error:#}", case.name);
            }
        }
        let (_, after) = fixture
            .user_access()
            .transact_readonly("test.job_policy.tools_after", |txn| {
                let tools_id = &tools_id;
                let node_did = &node_did;
                Box::pin(async move {
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn,
                        crate::Collection::Tools,
                        node_did,
                        tools_id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("selected Tools remains visible");
        let expected = if case.accepted {
            &case.candidate_integrations["plugins"][0]["job"]["policy"]["dynamic"]
        } else {
            &before["integrations"]["plugins"][0]["job"]["policy"]["dynamic"]
        };
        assert_eq!(
            &after["integrations"]["plugins"][0]["job"]["policy"]["dynamic"], expected,
            "{}",
            case.name
        );
        if case.add_schema_tool {
            assert_eq!(after["built_ins"], before["built_ins"], "{}", case.name);
            assert!(!fixture
                .db
                .node
                .nac_manager()
                .check_permission(
                    &actor,
                    crate::store::config_client::NodePermission::CollectionPatch,
                )
                .await
                .unwrap());
        }
        fixture.db.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn model_reselection_and_existing_sibling_grants_use_invoker_bounds() {
    let fixture = SelfConfigFixture::new("reselection-grant-bound").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_agent_relation_delete().await;
    fixture.grant_node_reader().await;
    let sibling = fixture.install_sibling("sibling").await;
    let granted = fixture.install_sibling("granted").await;
    fixture.grant_sibling_configuration(&sibling).await;
    fixture.grant_pack_install_to(&granted).await;
    let context = format!("{granted}:context");
    let core = SelfConfigCore::new(
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        sibling.clone(),
    )
    .unwrap()
    .with_lockout_agent_did(fixture.agent_did().to_owned())
    .with_agent_signer(fixture.agent.managed_agent_signer())
    .with_runtime_identity(Some(fixture.db.node_identity.clone()))
    .with_node_binding(Some(fixture.node_binding.clone()));
    for preview in [true, false] {
        let request = protect_working_agent(
            agent_request(&core, vec![("context_id".into(), Some(json!(context)))]),
            false,
            true,
        );
        let result = if preview {
            core.preview(request).await
        } else {
            core.apply(request).await
        };
        assert!(format!("{:#}", result.unwrap_err()).contains("cannot be self-granted"));
    }
    fixture.grant_pack_install().await;
    let holder = core.with_held_grants(ops::OperatorGrants { pack_install: true });
    holder
        .apply(protect_working_agent(
            agent_request(&holder, vec![("context_id".into(), Some(json!(context)))]),
            false,
            true,
        ))
        .await
        .unwrap();
    fixture
        .user_access
        .transact_write("test.grant_sibling_self_config", |txn| {
            let owner = fixture.node_did();
            let sibling = &sibling;
            Box::pin(async move {
                crate::store::config_client::grant_selected_self_configuration(txn, owner, sibling)
                    .await
            })
        })
        .await
        .unwrap();
    let sibling_doc = crate::load_agent(&fixture.db.node, &sibling)
        .await
        .unwrap()
        .unwrap();
    let sibling_signer = crate::store::identity::resolve_agent_identity(
        fixture.db.node_identity.as_ref(),
        &sibling_doc.identity_binding(),
    )
    .unwrap();
    let no_grant = SelfConfigCore::new(
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        sibling,
    )
    .unwrap()
    .with_agent_signer(sibling_signer.managed_agent_signer())
    .with_runtime_identity(Some(fixture.db.node_identity.clone()))
    .with_node_binding(Some(fixture.node_binding.clone()));
    no_grant
        .apply(tools_request(
            &no_grant,
            vec![("built_ins".into(), Some(json!({"enable_goal_tools":true})))],
        ))
        .await
        .unwrap();
    no_grant.apply(tools_request(&no_grant, vec![("self_config".into(), Some(json!({"enable_self_config":true,"enable_pack_install":true,"self_config_preview":true})))])).await.unwrap();
}

#[tokio::test]
async fn native_pack_local_install_inspection_selection_and_digest_fence() {
    let (workspace, artifact, archive) =
        crate::pack::plugin_lifecycle::tests::fixture("1.0.0", false);
    let home = tempfile::tempdir().unwrap();
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let fixture = SelfConfigFixture::new("native-pack").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let node = fixture.db.node.clone();
    let did = fixture.db.node_identity.did().to_owned();
    let mut cfg = config(&["tools", "agent"]);
    cfg.agent_did = fixture.agent_did().to_owned();
    cfg.enable_pack_install = true;
    cfg.process_ceiling.file_mode = crate::tools::surface::FileToolMode::ReadOnly;
    cfg.process_ceiling.root = Some(workspace.path().to_owned());
    let tools = fixture.tools_with_plugins(&cfg, plugins.clone());
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    tool.call(json!({"argv":["tools","update"],"set":{"host":{"root":workspace.path(),"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}}).to_string()).await.unwrap();
    tool.call(
        json!({"argv":["tools","update"],"set":{"integrations":{"lsp":{"timeout_secs":31}}}})
            .to_string(),
    )
    .await
    .unwrap();
    let path = artifact.to_str().unwrap();
    let preview: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "preview", "install", path])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], true);
    assert_eq!(preview["artifact_digest"], archive.digest());
    assert_eq!(
        preview["apply_with"]["argv"],
        json!(["pack", "install", archive.digest()])
    );
    assert!(crate::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    let wrong = format!("sha256:{}", "0".repeat(64));
    let error = config_call(&tools, &["pack", "install", path, "--digest", &wrong])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("digest changed"), "{error}");
    assert!(crate::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "install", path, "--digest", archive.digest()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["committed"], true);
    let inspect: Value = serde_json::from_str(
        &config_call(&tools, &["plugin", "get", "fixture/native_echo"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(inspect["selection"]["plugin"], "fixture/native_echo");
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    let before: Value =
        serde_json::from_str(&config_call(&tools, &["tools", "get"]).await.unwrap()).unwrap();
    assert!(
        !before.to_string().contains("native_echo"),
        "install must not select a tool"
    );
    let mut integrations = before["document"]["integrations"].clone();
    integrations
        .as_object_mut()
        .unwrap()
        .insert("plugins".into(), json!([inspect["selection"].clone()]));
    tool.call(json!({"argv":["tools","update"],"set":{"integrations":integrations}}).to_string())
        .await
        .unwrap();
    let readback = config_call(&tools, &["tools", "get"]).await.unwrap();
    assert!(readback.contains("fixture/native_echo"));
    let persisted: crate::documents::Tools = fixture
        .user_access
        .transact_readonly("test.self_config.read_native_pack_tools", |txn| {
            let did = did.clone();
            let tools_id = format!("{}:tools", fixture.agent_did());
            Box::pin(async move {
                let value = crate::store::config_client::read_desired_state_document_in_txn(
                    txn,
                    crate::Collection::Tools,
                    &did,
                    &tools_id,
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("persisted Tools document disappeared"))?;
                Ok::<_, anyhow::Error>(serde_json::from_value(value)?)
            })
        })
        .await
        .unwrap();
    assert_eq!(
        persisted
            .integrations
            .as_ref()
            .and_then(|integrations| integrations.lsp.as_ref())
            .and_then(|lsp| lsp.timeout_secs),
        Some(31),
        "selecting the plugin must preserve another configured integration"
    );
    let record = crate::plugin::store::read_record(home.path(), "fixture", "native_echo").unwrap();
    assert!(record.granted.is_none());
    let surface = crate::tools::surface::AgentToolSurfaceConfig::from_tools_document(
        fixture.agent_did(),
        &persisted,
        &crate::tools::surface::ToolCeiling::readonly_at(workspace.path()),
        Vec::new(),
    )
    .unwrap()
    .resolve(&node, &did, &plugins)
    .await
    .unwrap();
    let runtime =
        crate::tools::surface::ToolRuntimeContext::oneshot_with_node_did(node.clone(), &did)
            .with_plugins(plugins.clone());
    let fresh_tools = surface.build_tools(&runtime).await.unwrap();
    let fresh_tool = fresh_tools
        .iter()
        .find(|tool| tool.name() == "native_echo")
        .expect("a fresh ToolSurface built from persisted Tools should expose the selected plugin");
    let output: Value = serde_json::from_str(&fresh_tool.call("{}".into()).await.unwrap()).unwrap();
    assert_eq!(output, json!({"native_pack_echo":"ok"}));
    let (_next_dir, next_path, next) =
        crate::pack::plugin_lifecycle::tests::fixture("2.0.0", false);
    std::fs::copy(next_path, &artifact).unwrap();
    let error = config_call(
        &tools,
        &["pack", "update", path, "--digest", archive.digest()],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("digest changed"));
    config_call(&tools, &["pack", "update", path, "--digest", next.digest()])
        .await
        .unwrap();
    let status: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", "fixture/native_echo_pack"])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(status["installed"]["version"], "2.0.0");
    let removal = config_call(&tools, &["pack", "remove", "fixture/native_echo_pack"])
        .await
        .unwrap_err()
        .to_string();
    assert!(removal.contains("native removal"));
    assert_eq!(
        crate::plugin::store::read_record(home.path(), "fixture", "native_echo")
            .unwrap()
            .version,
        "2.0.0"
    );
}

#[tokio::test]
async fn native_pack_local_paths_and_approval_fail_closed() {
    let (workspace, artifact, _) = crate::pack::plugin_lifecycle::tests::fixture("1.0.0", true);
    let home = tempfile::tempdir().unwrap();
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let fixture = SelfConfigFixture::new("native-pack-refusals").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let mut cfg = config(&["tools"]);
    cfg.agent_did = fixture.agent_did().to_owned();
    cfg.enable_pack_install = true;
    cfg.process_ceiling.root = Some(workspace.path().to_owned());
    let tools = fixture.tools_with_plugins(&cfg, plugins.clone());
    let path = artifact.to_str().unwrap();
    assert!(config_call(&tools, &["pack", "get", path])
        .await
        .unwrap_err()
        .to_string()
        .contains("effective file read permission"));
    cfg.process_ceiling.file_mode = crate::tools::surface::FileToolMode::ReadOnly;
    let tools = fixture.tools_with_plugins(&cfg, plugins);
    assert!(config_call(&tools, &["pack", "get", path])
        .await
        .unwrap_err()
        .to_string()
        .contains("effective file read permission"));
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    tool.call(json!({"argv":["tools","update"],"set":{"host":{"root":workspace.path(),"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}}).to_string()).await.unwrap();
    let preview: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "preview", "install", path])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], false);
    assert_eq!(preview["apply_with"], Value::Null);
    let digest = preview["artifact_digest"].as_str().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    assert!(tool
        .call(
            json!({"argv":["pack","install",path],"options":{"digest":digest,"approved":true}})
                .to_string()
        )
        .await
        .is_err());
    assert!(
        config_call(&tools, &["pack", "install", path, "--digest", digest])
            .await
            .is_err()
    );
    assert!(crate::plugin::store::list_records(home.path())
        .unwrap()
        .is_empty());
    let outside = tempfile::tempdir().unwrap();
    let outside_path = outside.path().join("outside.pack");
    std::fs::copy(&artifact, &outside_path).unwrap();
    let error = config_call(&tools, &["pack", "get", outside_path.to_str().unwrap()])
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("outside the allowed tool root"));
    #[cfg(unix)]
    {
        let link = workspace.path().join("escape.pack");
        std::os::unix::fs::symlink(&outside_path, &link).unwrap();
        assert!(
            config_call(&tools, &["pack", "get", link.to_str().unwrap()])
                .await
                .unwrap_err()
                .to_string()
                .contains("outside the allowed tool root")
        );
    }
    let corrupt = workspace.path().join("corrupt.pack");
    std::fs::write(&corrupt, b"not a pack").unwrap();
    assert!(
        config_call(&tools, &["pack", "get", corrupt.to_str().unwrap()])
            .await
            .is_err()
    );
    assert!(
        config_call(&tools, &["pack", "get", workspace.path().to_str().unwrap()])
            .await
            .is_err()
    );
}

/// The local built lifecycle accepts the same pack kinds the registry
/// lifecycle accepts. A documents pack (documents + plugins + inference
/// slots) reached by explicit path must preview and install through the
/// canonical document installer, with plugin authority still fenced behind
/// operator blockers: the pack's plugin is refused at commit until the
/// grant exists in the plugin home, so the exact downloaded-style artifact
/// bytes flow through the same digest-pinned receipt.
#[tokio::test]
async fn native_pack_local_path_installs_mixed_documents_pack_with_fenced_plugin_authority() {
    let (fixture, directory) =
        crate::test_support::fixture_pack_copy("documents_fixture", &json!({}));
    let manifest_path = directory.join("manifest.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest.as_object_mut().unwrap().remove("schemas");
    // Make this a genuinely mixed pack: the documents pack ships a plugin
    // that asks for network authority, so the operator fence is exercised.
    // The fixture compiler replaces plugin artifacts, so the plugin is
    // written by hand after the copy.
    let plugins = json!([{
        "name": "fixture_notifier",
        "description": "Deterministic native pack fixture plugin asking for outbound HTTP",
        "artifact": "plugins/fixture_notifier.afb",
        "language": "rust",
        "input_schema": {"type": "object", "properties": {}, "additionalProperties": false},
        "manifold": {"fs": "None", "net": {"OutboundHttp": ["https://example.invalid"]}, "env": "None", "crypto": false, "child_process": false}
    }]);
    manifest["plugins"] = plugins.clone();
    manifest["assets"] = json!(manifest["assets"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .chain(std::iter::once(Value::String(
            "plugins/fixture_notifier.afb".into(),
        )))
        .collect::<Vec<_>>());
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let plugins_dir = directory.join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    let wat = crate::plugin::tests::constant_output_wat(br#"{"native_mixed_echo":"ok"}"#);
    std::fs::write(
        plugins_dir.join("fixture_notifier.afb"),
        crate::plugin::tests::build_plugin_afb(&wat),
    )
    .unwrap();

    // The artifact on disk is a built .pack, reached by explicit path.
    let (bytes, _) = crate::pack::archive::pack_dir(&directory).unwrap();
    let artifact = fixture.path().join("local_mixed.pack");
    std::fs::write(&artifact, &bytes).unwrap();

    let (home, plugins) = crate::test_support::home_with_pack_dir(&directory);
    let (agent_fixture, tools) =
        pack_tool_with_local_read("local-mixed-documents", plugins, fixture.path()).await;
    let node = &agent_fixture.db.node;
    let did = agent_fixture.db.node_identity.did();
    let slot = format!("worker={}:inference", agent_fixture.agent_did());

    // get: reports kind, digest and the operator blocker for the plugin.
    let get: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", artifact.to_str().unwrap()])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(get["manifest"]["kind"], "documents");
    assert_eq!(get["installable"], true);
    assert_eq!(get["source"], "store");
    let digest = get["artifact_digest"].as_str().unwrap().to_owned();
    assert_eq!(
        get["installation_blockers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|blocker| blocker["plugin"] == "fixture_notifier")
            .count(),
        1,
        "{get}"
    );

    // preview: no slot bound yet -> missing_inference_slots, not committed.
    let missing: Value = serde_json::from_str(
        &config_call(
            &tools,
            &["pack", "preview", "install", artifact.to_str().unwrap()],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(missing["ready"], false);
    assert_eq!(missing["missing_inference_slots"], json!(["worker"]));
    assert_eq!(missing["committed"], false);

    // preview with the slot bound: ready, digest-pinned apply_with, and the
    // plugin blocker still reported for the operator.
    let preview: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "preview",
                "install",
                artifact.to_str().unwrap(),
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], false, "{preview}");
    assert_eq!(preview["apply_with"], Value::Null);
    assert_eq!(preview["missing_inference_slots"], Value::Null);
    assert_eq!(
        preview["installation_blockers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|blocker| blocker["plugin"] == "fixture_notifier")
            .count(),
        1,
        "{preview}"
    );
    assert_eq!(preview["artifact_digest"], Value::String(digest.clone()));

    // Commit without the operator grant is refused; nothing installs.
    let refused = config_call(
        &tools,
        &[
            "pack",
            "install",
            artifact.to_str().unwrap(),
            "--digest",
            &digest,
            "--inference-slot",
            &slot,
        ],
    )
    .await
    .unwrap_err();
    assert!(
        refused.to_string().contains("operator authorization"),
        "{refused}"
    );
    let access = &agent_fixture.user_access;
    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );

    // The operator authorizes this exact artifact through the plugin grant
    // owner: the home's plugin store records a grant covering the declared
    // manifold. Model input cannot create it; the commit fence reads it.
    let plugin_declaration: crate::pack::PackPlugin =
        serde_json::from_value(manifest["plugins"][0].clone()).unwrap();
    let granted = crate::plugin::authority::declared_manifold(&plugin_declaration).unwrap();
    crate::plugin::store::write_record(
        home.path(),
        &crate::plugin::store::InstalledPlugin {
            namespace: "fixture".into(),
            name: "fixture_notifier".into(),
            version: "1.0.0".into(),
            digest: "sha256:operator-grant".into(),
            language: "rust".into(),
            declaration: plugin_declaration,
            granted: Some(granted),
            instructions: None,
            owner_pack_coordinate: None,
            owner_pack_digest: None,
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
        },
    )
    .unwrap();
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "install",
                artifact.to_str().unwrap(),
                "--digest",
                &digest,
                "--inference-slot",
                &slot,
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        installed["effective"]["digest"],
        Value::String(digest.clone())
    );
    assert!(node.get_collection("FixtureJob").unwrap().is_some());
    let record =
        crate::plugin::store::read_record(home.path(), "fixture", "fixture_notifier").unwrap();
    assert_eq!(
        record.owner_pack_coordinate.as_deref(),
        Some("fixture/documents_fixture")
    );
    assert!(
        record.granted.is_some(),
        "the operator grant the fence required is on the installed record"
    );
}

/// A plugins-only pack keeps the pre-existing local path behavior: preview
/// reports blockers under an untrusted namespace and commit stays blocked
/// until the operator grant exists in the plugin home.
#[tokio::test]
async fn native_pack_local_path_plugins_only_still_requires_operator_grant() {
    let (workspace, artifact, _) = crate::pack::plugin_lifecycle::tests::fixture("1.0.0", true);
    let home = tempfile::tempdir().unwrap();
    let plugins = Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let workspace = &workspace;
    let (_agent_fixture, tools) =
        pack_tool_with_local_read("local-plugins-only", plugins, workspace.path()).await;
    let path = artifact.to_str().unwrap().to_owned();

    let preview: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "preview", "install", &path])
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["ready"], false, "{preview}");
    assert_eq!(preview["apply_with"], Value::Null);
    assert!(!preview["installation_blockers"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(
        crate::plugin::store::list_records(home.path())
            .unwrap()
            .is_empty(),
        "preview must not publish plugins"
    );
}

/// A pack tool whose invoking agent holds effective file read authority over
/// `root` (process ceiling ReadOnly plus an effective tools grant), the
/// authority the local built-artifact path requires before it opens a .pack.
async fn pack_tool_with_local_read(
    label: &str,
    plugins: Arc<crate::plugin::executor::PluginExecutor>,
    root: &std::path::Path,
) -> (SelfConfigFixture, Vec<Box<dyn ToolDyn>>) {
    let fixture = SelfConfigFixture::new(label).await;
    fixture.grant_selected_configuration().await;
    fixture.grant_pack_install().await;
    let mut tool_config = config(&["tools"]);
    tool_config.agent_did = fixture.agent_did().to_owned();
    tool_config.enable_pack_install = true;
    tool_config.process_ceiling.file_mode = crate::tools::surface::FileToolMode::ReadOnly;
    tool_config.process_ceiling.root = Some(root.to_owned());
    let tools = fixture.tools_with_plugins(&tool_config, plugins);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .unwrap();
    tool.call(json!({"argv":["tools","update"],"set":{"host":{"root":root,"files":{"mode":"ReadOnly"},"bash":{"mode":"Off"}}}}).to_string()).await.unwrap();
    (fixture, tools)
}

/// Installs two ChatGptCodex OAuth backends for `owner` — the provider's
/// default account and another account of the same provider — with one
/// inference profile each, plus the enabled credentials the default-account
/// resolver needs to keep the account-free reference as the provider
/// default. Binding to `p-default` is allowed; `p-other` selects another
/// account and must be refused.
async fn install_chat_account_fences(fixture: &SelfConfigFixture) {
    let owner = fixture.db.node_identity.did();
    let access = &fixture.user_access;
    let node = &fixture.db.node;
    for (id, auth) in [
        ("chat-original", json!({"kind":"node_oauth"})),
        (
            "chat-b",
            json!({"kind":"node_oauth","account_ref":"acct-b"}),
        ),
    ] {
        let backend: crate::InferenceBackend = serde_json::from_value(json!({
            "node_did": owner, "backend_id": id, "name": id,
            "provider_kind": "ChatGptCodex", "endpoint": "http://127.0.0.1:1/v1",
            "auth": auth,
        }))
        .unwrap();
        crate::store::config_client::write_inference_backend_document(access, &backend)
            .await
            .unwrap();
    }
    for (id, backend) in [("p-default", "chat-original"), ("p-other", "chat-b")] {
        let profile: crate::documents::InferenceProfile = serde_json::from_value(json!({
            "node_did": owner, "profile_id": id, "backend_id": backend,
            "model_name": "test-model",
        }))
        .unwrap();
        crate::store::config_client::write_inference_profile_document(access, &profile)
            .await
            .unwrap();
    }
    let provider = crate::inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER;
    let base_id = crate::inference::auth::credential::oauth_credential_id(owner, provider);
    for (account_ref, connected) in [(None, false), (Some("acct-b"), true)] {
        let credential = crate::inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: match account_ref {
                Some(account_ref) => format!("{base_id}:{account_ref}"),
                None => base_id.clone(),
            },
            node_did: owner.to_string(),
            provider: provider.to_string(),
            access_token: "access-TEST".into(),
            refresh_token: "refresh-TEST".into(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
            account_ref: account_ref.map(str::to_string),
            connected_at: connected.then(chrono::Utc::now),
            provider_account_key: None,
            label: None,
        };
        crate::inference::auth::credential::upsert_oauth_credential(node, &credential)
            .await
            .unwrap();
    }
}

/// A documents pack enforces inference account authority: a slot bound to a
/// profile whose backend selects another
/// account of one provider is refused at preview and at commit — before any
/// schema or installation side effect — while the provider's default account
/// and an account-free backend install.
#[tokio::test]
async fn pack_documents_install_refuses_non_default_account_inference_bindings() {
    let (_home, plugins) = crate::test_support::home_with_fixture_pack("documents_fixture");
    let (fixture, tools) = signed_pack_tool("documents-account-fence", plugins).await;
    install_chat_account_fences(&fixture).await;
    let node = &fixture.db.node;
    let did = fixture.db.node_identity.did();
    let access = &fixture.user_access;
    let get: Value = serde_json::from_str(
        &config_call(&tools, &["pack", "get", "fixture/documents_fixture"])
            .await
            .unwrap(),
    )
    .unwrap();
    let digest = get["artifact_digest"].as_str().unwrap().to_owned();

    // Preview refuses the non-default account binding.
    let refused = config_call(
        &tools,
        &[
            "pack",
            "preview",
            "install",
            "fixture/documents_fixture",
            "--inference-slot",
            "worker=p-other",
        ],
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("selects another"), "{refused}");
    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        node.get_collection("FixtureJob").unwrap().is_none(),
        "refused preview must not publish the pack's schemas"
    );

    // Commit refuses the same binding even with the pinned digest.
    let refused = config_call(
        &tools,
        &[
            "pack",
            "install",
            "fixture/documents_fixture",
            "--digest",
            &digest,
            "--inference-slot",
            "worker=p-other",
        ],
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("selects another"), "{refused}");
    assert!(
        crate::pack::read_installed_pack(access, did, "fixture/documents_fixture")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        node.get_collection("FixtureJob").unwrap().is_none(),
        "refused commit must not publish the pack's schemas"
    );

    // The provider's default account and the account-free setup backend are
    // both admitted, and the pack installs under the pinned digest.
    let current_profile = format!("{}:inference", fixture.agent_did());
    for profile in ["p-default", current_profile.as_str()] {
        let preview: Value = serde_json::from_str(
            &config_call(
                &tools,
                &[
                    "pack",
                    "preview",
                    "install",
                    "fixture/documents_fixture",
                    "--inference-slot",
                    &format!("worker={profile}"),
                ],
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(preview["ready"], true, "{profile}: {preview}");
    }
    let installed: Value = serde_json::from_str(
        &config_call(
            &tools,
            &[
                "pack",
                "install",
                "fixture/documents_fixture",
                "--digest",
                &digest,
                "--inference-slot",
                "worker=p-default",
            ],
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(installed["effective"]["digest"], Value::String(digest));
    assert!(node.get_collection("FixtureJob").unwrap().is_some());
}
