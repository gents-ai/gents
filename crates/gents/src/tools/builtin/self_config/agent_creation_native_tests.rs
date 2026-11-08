use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::{agent_management, ConfigureAgentParams, SelfConfigCore, StringUpdate};
use crate::store::config_client::{
    ConfigAccess, ConfigurationNodeBinding, DesiredStateApplyDocument, DesiredStateApplyPlan,
    GraphqlEndpoint, NodePermission,
};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use crate::Collection;

/// The User publishes a signed client request for `agent`, which verifies and
/// claims it as itself; the claim's tool context carries the admitted client.
async fn claimed_client_tool_context(
    node: &Arc<defra_node::Node>,
    runtime: Arc<KeyIdentity>,
    user: &KeyIdentity,
    user_access: &ConfigAccess,
    agent: &dyn NodeIdentity,
    request_id: &str,
) -> Result<(
    crate::request::lifecycle::RequestLifecycle,
    crate::inference::llm::tool::ToolRequestContext,
)> {
    use crate::request::lifecycle::{ClaimOutcome, ExecutionOrigin, RequestLifecycle};
    let spec = crate::request::lifecycle::RequestSpec::new(
        gents_protocol::request_admission::RequestPurpose::Normal,
        crate::request::lifecycle::RequestIdentity {
            requester_did: Some(user.did().to_owned()),
            request_id: request_id.to_owned(),
            node_did: runtime.did().to_owned(),
            agent_did: agent.did().to_owned(),
            session_id: format!("{request_id}-session"),
            content: "File a notice".to_owned(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        },
        gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
    );
    let create = crate::request::lifecycle::build_signed_client_request(
        user_access,
        spec,
        crate::request::lifecycle::RequestSigner::Identity(user),
    )
    .await?;
    crate::request::lifecycle::submit_client_request(user_access, &create).await?;
    let agent_access = ConfigAccess::local_managed_agent(node.clone(), agent.did().parse()?, agent)?;
    let response = agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{ {} }} }}"#,
            crate::store::graphql::escape_graphql_string(request_id),
            gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await?;
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone())?;
    let request = gents_protocol::row::AgentRequest::try_from(row)?;
    let (_authority_owner, authority) =
        crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        runtime.clone(),
        authority,
    );
    let (request, admission) = verifier.verify_fresh_for_claim(&request, agent.did()).await?;
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        agent.did(),
        runtime.did(),
        request,
        60,
        ExecutionOrigin::Interactive,
        String::new(),
    );
    lifecycle.bind_managed_agent(agent)?;
    lifecycle.set_client_claim_admission(admission);
    anyhow::ensure!(
        matches!(lifecycle.claim_with_identity().await?, ClaimOutcome::Claimed),
        "the selected Agent claims the User's request"
    );
    let context = lifecycle.claimed_tool_context()?;
    Ok((lifecycle, context))
}

async fn cached_surface_usable(
    access: &ConfigAccess,
    owner: &str,
    expected: &crate::documents::SurfaceToolDecl,
) -> bool {
    access
        .transact_readonly("test.surface.cached_use", |txn| {
            Box::pin(async move {
                crate::documents::revalidate_datastore_tool_in_txn(
                    txn,
                    owner,
                    "fixture/surface",
                    expected,
                )
                .await
            })
        })
        .await
        .is_ok()
}

async fn assert_pack_policy_observation(
    admin: &ConfigAccess,
    case: &Value,
    actor: &identity::Did,
    doc_id: &str,
    permission: &'static str,
    expected_field: &str,
) -> Result<()> {
    let decisions = admin
        .transact_readonly("test.pack.policy_observation", |txn| {
            let actor = actor.clone();
            let doc_id = doc_id.to_owned();
            Box::pin(async move {
                let node_permission = match permission {
                    "read" => NodePermission::DocumentRead,
                    "update" => NodePermission::DocumentUpdate,
                    "delete" => NodePermission::DocumentDelete,
                    _ => anyhow::bail!("unknown modeled permission"),
                };
                Ok::<_, anyhow::Error>(
                    txn.observe_document_access(
                        &actor,
                        "PackInstallation",
                        &[doc_id],
                        defra_node::DocumentAccessMode::Named {
                            permission: permission.into(),
                            node_permission,
                        },
                    )
                    .await?,
                )
            })
        })
        .await?;
    let actual = decisions == [defra_node::DocumentAccessDecision::Allowed];
    let expected = case[expected_field]
        .as_bool()
        .context("modeled PackInstallation access is missing")?;
    assert_eq!(actual, expected, "{}: {expected_field}", case["name"]);
    Ok(())
}

async fn assert_pack_policy_case(
    admin: &ConfigAccess,
    case: &Value,
    installer: &identity::Did,
    user: &identity::Did,
    runtime: &identity::Did,
    sibling: &identity::Did,
    doc: &str,
) -> Result<()> {
    for (actor, permission, field) in [
        (installer, "read", "installer_read"),
        (installer, "update", "installer_update"),
        (user, "read", "user_read"),
        (user, "update", "user_update"),
        (user, "delete", "user_delete"),
        (runtime, "read", "runtime_read"),
        (sibling, "read", "other_read"),
    ] {
        assert_pack_policy_observation(admin, case, actor, doc, permission, field).await?;
    }
    Ok(())
}

struct UnmanagedSigner(Arc<dyn NodeIdentity>);

#[async_trait::async_trait]
impl NodeIdentity for UnmanagedSigner {
    fn did(&self) -> &str {
        self.0.did()
    }
    async fn sign(
        &self,
        payload: &[u8],
    ) -> std::result::Result<Vec<u8>, crate::store::identity::IdentityError> {
        self.0.sign(payload).await
    }
    async fn verify(
        &self,
        did: &str,
        payload: &[u8],
        signature: &[u8],
    ) -> std::result::Result<bool, crate::store::identity::IdentityError> {
        self.0.verify(did, payload, signature).await
    }
    fn service_account(&self) -> Option<&crate::store::identity::ServiceAccount> {
        self.0.service_account()
    }
}

async fn user_signed_node(
    endpoint: &GraphqlEndpoint,
    runtime_did: &str,
    user_did: &str,
) -> Result<String> {
    let signing =
        defra_core::signing::get_identity(user_did).context("test user signer missing")?;
    let input = json!({"node_did": runtime_did});
    let input_for_graph = input.clone();
    let mut graph = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut schemas = query::parse_sdl(gents_protocol::schemas::NODE)?;
        Ok(db::block::builder::ClientCreateSignatures::from_input(
            &storage::Crdts::default(),
            &schemas.remove(0),
            &input_for_graph,
            &signing,
        )?)
    })
    .await??;
    let mut url = reqwest::Url::parse(endpoint.url())?;
    anyhow::ensure!(url.set_scheme("ws").is_ok(), "test endpoint must be HTTP");
    url.set_path(&format!("{}/client-sign", url.path()));
    let request = defra_http::client_signed_create::websocket_request(
        url.as_str(),
        &endpoint
            .authorization()?
            .context("test user authorization missing")?,
    )?;
    let (socket, _) = tokio_tungstenite::connect_async_with_config(
        request,
        Some(defra_http::client_signed_create::websocket_config()),
        false,
    )
    .await?;
    let query = query::QueryRequest::new(
        "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
    )
    .with_variables(json!({"input": input}));
    let response =
        defra_http::client_signed_create::execute_with_socket(socket, query, None, &mut graph)
            .await?;
    crate::store::graphql::created_doc_id(&serde_json::to_value(response)?, "Node")
        .map_err(Into::into)
}

#[tokio::test]
async fn agent_creates_and_configures_child_with_native_ownership() -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = Arc::new(KeyIdentity::load_or_create(
        home.path().join("runtime"),
        None,
    )?);
    let user = KeyIdentity::load_or_create(home.path().join("user"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    // The production runtime projector derives each live Agent's baseline
    // node grants from its document; the creator reads its own row through it.
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .with_document_projector(crate::store::local::document_projector(
                runtime.clone(),
                std::sync::Arc::new(crate::tools::surface::ToolCeiling::meta_only()),
            )?)
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    crate::store::schema::ensure_runtime_schemas(&node).await?;
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    let user_did = identity::Did::new(user.did())?;
    crate::store::identity::GrantSet::OperatorUser
        .grant(&admin, &user_did)
        .await?;
    let endpoint =
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates();
    let node_doc = user_signed_node(&endpoint, runtime.did(), user.did())
        .await
        .context("publishing the user-owned Node")?;
    let user_access = ConfigAccess::Graphql(endpoint);
    user_access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let creator = crate::store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await?;
    // The creator is the Node's default Agent: as `gents init` does, the
    // runtime issues it `policyDelegate`, which is what lets its creates
    // project the new Agent's baseline (`Identity.GrantSets`
    // `default_agent_publishes_agent`, `baseline_agent_cannot_publish_agent`).
    crate::store::identity::GrantSet::PolicyDelegate
        .grant(&admin, &identity::Did::new(&creator.agent_did)?)
        .await?;
    let sibling = crate::store::identity::provision_agent_identity_authorized(
        &node,
        runtime.as_ref(),
        &user_did,
    )
    .await?;
    let mut documents = Vec::new();
    let mut add = |collection, value: Value| {
        documents.push(DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        })
    };
    add(
        Collection::InferenceBackend,
        json!({"node_did":runtime.did(),"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}}),
    );
    add(
        Collection::InferenceProfile,
        json!({"node_did":runtime.did(),"profile_id":"profile","backend_id":"backend","model_name":"test"}),
    );
    for binding in [&creator, &sibling] {
        let context = format!("{}:context", binding.agent_did);
        let tools = format!("{}:tools", binding.agent_did);
        // The creator's Tools select the config tool it runs, so the runtime
        // projection holds that tool's native operations (pack removal).
        let mut tools_document = json!({"node_did":runtime.did(),"tools_id":tools});
        if binding.agent_did == creator.agent_did {
            tools_document["self_config"] = json!({
                "enable_self_config": true,
                "self_config_categories": ["agent", "tools", "node"],
                "enable_pack_install": true
            });
        }
        add(Collection::Tools, tools_document);
        add(
            Collection::AgentContext,
            json!({"node_did":runtime.did(),"context_id":context,"tools_id":tools}),
        );
        add(
            Collection::Agent,
            json!({"node_did":runtime.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"display_name":"Worker","context_id":context,"inference_profile_id":"profile"}),
        );
    }
    let plan = DesiredStateApplyPlan::new(documents)?;
    user_access
        .transact_write("test.agent_creator.seed", |txn| {
            let plan = &plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .context("seeding the user-owned Agent configuration")?;
    user_access.transact_write("test.agent_creator.select_default", |txn| {
        let node_doc = &node_doc;
        let creator = &creator;
        Box::pin(async move {
            let mutation = format!("mutation($input: NodeMutationInputArg!) {{ update_Node(docID: \"{}\", input: $input) {{ _docID }} }}", crate::store::graphql::escape_graphql_string(node_doc));
            txn.execute_client_signed_update(&mutation, &json!({"input": {"default_agent_did": creator.agent_did}}), gents_protocol::schemas::NODE).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await?;
    let signer = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &creator)?;
    let node_binding = ConfigurationNodeBinding {
        node_did: identity::Did::new(runtime.did())?,
        doc_id: node_doc.clone(),
        enabled: true,
        default_agent_did: None,
    };
    let core = SelfConfigCore::new(
        node.clone(),
        runtime.did().to_owned(),
        creator.agent_did.clone(),
    )?
    .with_runtime_identity(Some(runtime.clone()))
    .with_agent_signer(signer.managed_agent_signer())
    .with_node_binding(Some(node_binding.clone()));
    // Creating an Agent needs the agent catalog grant (`SelfConfig.mayCreateAgent`).
    let mut tool_config = super::tests::config(&["agent", "tools", "node"]);
    tool_config.agent_did = creator.agent_did.clone();
    let tools = super::build_self_config_tools_with_runtime(
        Arc::default(),
        node.clone(),
        runtime.did().to_owned(),
        Some(signer.clone()),
        Some(runtime.clone()),
        &tool_config,
        Arc::default(),
        Some(node_binding),
    );
    let tool = tools
        .iter()
        .find(|tool| tool.name() == super::CONFIG_TOOL_NAME)
        .context("configuration tool missing")?;
    let creator_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(&creator.agent_did)?,
        signer.as_ref(),
    )?;
    user_access
        .transact_write("test.agent_creator.self_config_grants", |txn| {
            let creator = &creator;
            let runtime = &runtime;
            Box::pin(async move {
                crate::store::config_client::grant_selected_self_configuration(
                    txn,
                    runtime.did(),
                    &creator.agent_did,
                )
                .await
            })
        })
        .await?;
    let tools_id = format!("{}:tools", creator.agent_did);
    tool.call(json!({"argv":["tools","update"],"set":{"display_name":"configured"}}).to_string())
        .await
        .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    let tools_query = format!(
        r#"{{ Tools(filter: {{tools_id: {{_eq: "{}"}}}}) {{ _docID display_name }} }}"#,
        crate::store::graphql::escape_graphql_string(&tools_id)
    );
    let configured = user_access.execute(&tools_query).await?;
    assert_eq!(configured["data"]["Tools"][0]["display_name"], "configured");
    user_access
        .set_agent_self_config_permission(
            runtime.did(),
            &creator.agent_did,
            crate::store::config_client::AgentSelfConfigPermission::Revoke,
        )
        .await?;
    assert!(tool
        .call(json!({"argv":["tools","update"],"set":{"display_name":"revoked"}}).to_string())
        .await
        .is_err());
    assert_eq!(user_access.execute(&tools_query).await?, configured);
    user_access
        .set_agent_self_config_permission(
            runtime.did(),
            &creator.agent_did,
            crate::store::config_client::AgentSelfConfigPermission::Grant,
        )
        .await?;
    assert_eq!(
        creator_access.execute("{ Node { _docID } }").await?["data"]["Node"],
        json!([])
    );
    let listed: Value = serde_json::from_str(
        &tool
            .call(json!({"argv":["agent","list"]}).to_string())
            .await
            .map_err(|error| anyhow::anyhow!("{error:#}"))?,
    )?;
    assert_eq!(listed["default_agent_did"], creator.agent_did);
    assert!(listed["agents"]
        .as_array()
        .context("agent list omitted rows")?
        .iter()
        .any(|agent| agent["agent_did"] == creator.agent_did && agent["is_default"] == true));
    let selected: Value = serde_json::from_str(
        &tool
            .call(json!({"argv":["agent","get", creator.agent_did]}).to_string())
            .await
            .map_err(|error| anyhow::anyhow!("{error:#}"))?,
    )?;
    assert_eq!(selected["is_default"], true);
    let runtime_actor = identity::Did::new(runtime.did())?;
    user_access
        .delete_actor_relationship("Node", &node_doc, "runtime_reader", &runtime_actor)
        .await?;
    assert!(tool
        .call(json!({"argv":["agent","list"]}).to_string())
        .await
        .is_err());
    user_access
        .add_actor_relationship("Node", &node_doc, "runtime_reader", &runtime_actor)
        .await?;
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .identity_agent_creator_cases
            .clone()
    })
    .await?;
    let other_runtime = KeyIdentity::load_or_create(home.path().join("other-runtime"), None)?;
    let sibling_signer =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &sibling)?;
    let unmanaged = UnmanagedSigner(signer.clone());
    for name in [
        "authorized-agent",
        "foreign-runtime",
        "foreign-signer",
        "missing-signer",
    ] {
        let case = cases
            .iter()
            .find(|case| case["name"] == name)
            .context("modeled creator case missing")?;
        let (root, actor): (&dyn NodeIdentity, &dyn NodeIdentity) = match name {
            "foreign-runtime" => (&other_runtime, signer.as_ref()),
            "foreign-signer" => (runtime.as_ref(), sibling_signer.as_ref()),
            "missing-signer" => (runtime.as_ref(), &unmanaged),
            _ => (runtime.as_ref(), signer.as_ref()),
        };
        let result = crate::store::identity::provision_agent_identity_for_agent(
            &node,
            root,
            actor,
            &creator.agent_did,
        )
        .await;
        assert_eq!(result.is_ok(), case["allowed"].as_bool().unwrap(), "{name}");
    }
    let create = ConfigureAgentParams {
        action: "create".into(),
        display_name: StringUpdate::Set("Child".into()),
        system_prompt: StringUpdate::Set("Work".into()),
        profile_id: StringUpdate::Set("profile".into()),
        ..Default::default()
    };
    let first: Value = serde_json::from_str(
        &tool
            .call(
                json!({
                    "argv":["agent","create"],
                    "options":{"display-name":"Child", "system-prompt":"Work", "profile":"profile"}
                })
                .to_string(),
            )
            .await
            .map_err(|error| anyhow::anyhow!("{error:#}"))?,
    )?;
    let second: Value = serde_json::from_str(
        &agent_management::model_agent_mutate(&core, signer.as_ref(), &create).await?,
    )?;
    let child = first["agent_did"]
        .as_str()
        .context("created Agent DID missing")?;
    assert_ne!(first["agent_did"], second["agent_did"]);
    assert_ne!(
        first["agent"]["creation_nonce"],
        second["agent"]["creation_nonce"]
    );
    let clone = ConfigureAgentParams {
        action: "clone".into(),
        display_name: StringUpdate::Set("Cloned child".into()),
        clone_from: Some(creator.agent_did.clone()),
        profile_id: StringUpdate::Set("profile".into()),
        ..Default::default()
    };
    let cloned: Value = serde_json::from_str(
        &agent_management::model_agent_mutate(&core, signer.as_ref(), &clone).await?,
    )?;
    assert_ne!(cloned["agent_did"], first["agent_did"]);
    assert_ne!(
        cloned["agent"]["creation_nonce"],
        first["agent"]["creation_nonce"]
    );
    let rename = ConfigureAgentParams {
        action: "edit".into(),
        agent_did: Some(child.to_owned()),
        display_name: StringUpdate::Set("Renamed child".into()),
        ..Default::default()
    };
    let edited: Value = serde_json::from_str(
        &agent_management::model_agent_mutate(&core, signer.as_ref(), &rename).await?,
    )?;
    assert_eq!(
        edited["agent"]["creation_nonce"],
        first["agent"]["creation_nonce"]
    );
    tool.call(
        json!({"argv":["agent","update"], "target_id":child,
        "set":{"display_name":"Tool-edited child"}})
        .to_string(),
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error:#}"))?;
    let sibling_query = format!(
        r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID display_name }} }}"#,
        crate::store::graphql::escape_graphql_string(&sibling.agent_did)
    );
    let sibling_before = user_access.execute(&sibling_query).await?;
    assert!(tool
        .call(
            json!({"argv":["agent","update"], "target_id":sibling.agent_did,
        "set":{"display_name":"Not allowed"}})
            .to_string()
        )
        .await
        .is_err());
    assert_eq!(user_access.execute(&sibling_query).await?, sibling_before);
    let query = format!(
        r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID display_name }} }}"#,
        crate::store::graphql::escape_graphql_string(child)
    );
    let creator_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(signer.did())?,
        signer.as_ref(),
    )?;
    let creator_rows = creator_access.execute(&query).await?;
    let child_doc = creator_rows["data"]["Agent"][0]["_docID"]
        .as_str()
        .context("creator cannot read child")?;
    assert_eq!(
        creator_rows["data"]["Agent"][0]["display_name"],
        "Tool-edited child"
    );
    let user_rows = user_access.execute(&query).await?;
    assert_eq!(user_rows["data"]["Agent"][0]["_docID"], child_doc);
    let context_id = first["agent"]["context_id"]
        .as_str()
        .context("child context missing")?;
    let context_query = format!(
        r#"{{ AgentContext(filter: {{context_id: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
        crate::store::graphql::escape_graphql_string(context_id)
    );
    assert_eq!(
        creator_access.execute(&context_query).await?["data"]["AgentContext"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        user_access.execute(&context_query).await?["data"]["AgentContext"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let sibling_access =
        ConfigAccess::local_as(node.clone(), identity::Did::new(&sibling.agent_did)?);
    assert_eq!(
        sibling_access.execute(&query).await?["data"]["Agent"],
        json!([])
    );
    assert_eq!(
        sibling_access.execute(&context_query).await?["data"]["AgentContext"],
        json!([])
    );
    let grants = admin
        .transact_readonly("test.agent_creator.owner", |txn| {
            let user_did = &user_did;
            let child_doc = child_doc.to_owned();
            Box::pin(async move {
                txn.observe_document_access(
                    user_did,
                    "Agent",
                    &[child_doc],
                    defra_node::DocumentAccessMode::Named {
                        permission: "update".into(),
                        node_permission: NodePermission::DocumentUpdate,
                    },
                )
                .await
            })
        })
        .await?;
    assert_eq!(grants, vec![defra_node::DocumentAccessDecision::Allowed]);
    let commits =
        crate::store::graphql::composite_commits(&node, child_doc, "test child genesis").await?;
    let creator_signer =
        crate::store::identity::commit_signer_identity_for_did(&creator.agent_did)?;
    let genesis = commits
        .iter()
        .min_by_key(|commit| commit.height)
        .context("child genesis missing")?;
    assert_eq!(
        genesis
            .signature
            .as_ref()
            .map(|signature| signature.identity.as_str()),
        Some(creator_signer.as_str())
    );
    assert_eq!(
        creator_access.execute("{ Node { _docID } }").await?["data"]["Node"],
        json!([])
    );
    let (_fixture, _, archive) = crate::pack::plugin_lifecycle::tests::fixture("1.0.0", false);
    let plugin_home = tempfile::tempdir()?;
    let distribution =
        super::pack_installer::ConfigPackDistribution(crate::pack::resolve::ResolvedNamedPack {
            archive,
            from: crate::pack::resolve::ResolvedFrom::Store,
        });
    let installer = super::pack_installer::PackInstaller {
        core: core.clone(),
        node: node.clone(),
        actor: Some(signer.clone()),
        home: Some(plugin_home.path().to_owned()),
    };
    let args = super::pack_installer::PackInstallParams {
        package: "fixture/native_echo_pack".into(),
        variables: Default::default(),
        inference_slots: Default::default(),
        expected_digest: None,
    };
    let preview: Value = serde_json::from_str(
        &installer
            .plugins_pack("install", &args, &distribution, false)
            .await
            .context("previewing the Agent's plugin pack")?,
    )?;
    assert_eq!(preview["ready"], true);
    let installed: Value = serde_json::from_str(
        &installer
            .plugins_pack("install", &args, &distribution, true)
            .await
            .context("installing the Agent's plugin pack")?,
    )?;
    assert_eq!(installed["committed"], true);
    let record = creator_access
        .execute("{ PackInstallation(filter: {coordinate: {_eq: \"fixture/native_echo_pack\"}}) { _docID } }")
        .await?;
    let record_doc = record["data"]["PackInstallation"][0]["_docID"]
        .as_str()
        .context("plugin installation record missing")?;
    let commits = crate::store::graphql::composite_commits(
        &node,
        record_doc,
        "test plugin installation genesis",
    )
    .await?;
    let genesis = commits
        .iter()
        .min_by_key(|commit| commit.height)
        .context("plugin installation genesis missing")?;
    assert_eq!(
        genesis
            .signature
            .as_ref()
            .map(|signature| signature.identity.as_str()),
        Some(creator_signer.as_str())
    );
    let mismatch = crate::pack::record_plugin_store_change_as(
        &creator_access,
        runtime.did(),
        Some(identity::Did::new(user.did())?),
        plugin_home.path(),
        "fixture/native_echo_pack",
        None,
    )
    .await
    .unwrap_err();
    let crate::pack::PluginStoreChangeError::Rejected(error) = mismatch else {
        anyhow::bail!("mismatched actor was not rejected");
    };
    assert!(matches!(
        error.downcast_ref::<crate::store::config_client::ConfigClientError>(),
        Some(crate::store::config_client::ConfigClientError::TransactionActorMismatch)
    ));
    let after = creator_access
        .execute("{ PackInstallation(filter: {coordinate: {_eq: \"fixture/native_echo_pack\"}}) { _docID } }")
        .await?;
    assert_eq!(after, record);
    assert_eq!(
        creator_access.execute("{ Node { _docID } }").await?["data"]["Node"],
        json!([])
    );
    let modeled = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .context("configuration policy model is missing")?;
    assert_eq!(modeled.configuration_pack_installation_cases.len(), 6);
    assert_eq!(modeled.configuration_pack_removal_cases.len(), 5);
    let model_case = |name: &str| {
        modeled
            .configuration_pack_installation_cases
            .iter()
            .find(|case| case["name"] == name)
            .with_context(|| format!("missing modeled pack case {name}"))
    };
    let agent_did = identity::Did::new(signer.did())?;
    let runtime_did = identity::Did::new(runtime.did())?;
    let sibling_did = identity::Did::new(&sibling.agent_did)?;
    assert_pack_policy_case(
        &admin,
        model_case("agent-created")?,
        &agent_did,
        &user_did,
        &runtime_did,
        &sibling_did,
        record_doc,
    )
    .await?;
    let sibling_writer = ConfigAccess::local_managed_agent(
        node.clone(),
        sibling_did.clone(),
        sibling_signer.as_ref(),
    )?;
    let hidden_tool = format!("{}:tools", creator.agent_did);
    let tool_query = format!(
        "{{ Tools(filter: {{tools_id: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
        crate::store::graphql::escape_graphql_string(&hidden_tool),
    );
    assert_eq!(
        sibling_writer.execute(&tool_query).await?["data"]["Tools"],
        json!([])
    );
    assert_eq!(
        user_access.execute(&tool_query).await?["data"]["Tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let hidden_record: String = creator_access.transact_write("test.pack.hidden_child", |txn| {
        let hidden_tool = &hidden_tool;
        let sibling_did = &sibling_did;
        let node_doc = &node_doc;
        let owner = runtime.did();
        Box::pin(async move {
            let response = txn.execute_client_signed_create(
                "mutation($input: PackInstallationMutationInputArg!) { create_PackInstallation(input: $input) { _docID } }",
                &json!({"input": {"node_did": owner, "coordinate": "fixture/hidden-child",
                    "documents": [{"collection": "Tools", "id": hidden_tool,
                        "digest": "unused", "created": true}], "explicit": true}}),
                gents_protocol::schemas::PACK_INSTALLATION,
            ).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "PackInstallation")?;
            txn.add_resource_relationship("PackInstallation", &doc, "parent", "nodes", node_doc).await?;
            txn.add_actor_relationship("PackInstallation", &doc, "writer", sibling_did).await?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await?;
    let hidden_case = modeled
        .configuration_pack_cleanup_cases
        .iter()
        .find(|case| case["name"] == "unavailable-first")
        .context("missing cleanup model case")?;
    assert_eq!(hidden_case["expected"], false);
    let error = crate::pack::remove_pack(
        &sibling_writer,
        runtime.did(),
        "fixture/hidden-child",
        crate::pack::DriftPolicy::Refuse,
    )
    .await
    .unwrap_err();
    assert!(
        gents_protocol::error_chain::find_map_source(error.as_ref(), |cause| {
            matches!(cause.downcast_ref::<crate::pack::PackRemovalError>(),
            Some(crate::pack::PackRemovalError::OwnedDocumentUnavailable { collection, id })
            if collection == "Tools" && id == &hidden_tool)
            .then_some(())
        })
        .is_some()
    );
    let retained = user_access
        .execute(&format!(
            "{{ PackInstallation(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
            crate::store::graphql::escape_graphql_string(&hidden_record),
        ))
        .await?;
    assert_eq!(
        retained["data"]["PackInstallation"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        user_access.execute(&tool_query).await?["data"]["Tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    admin
        .add_collection("type FixtureEntry { name: String }")
        .await?;
    let declaration = crate::documents::SurfaceToolDecl::Query(crate::documents::QueryToolDecl {
        tool_name: "find_fixture_entries".into(),
        collection: "FixtureEntry".into(),
        description: "Find entries".into(),
        fields: vec!["name".into()],
        filter_fields: vec![],
    });
    let crate::documents::SurfaceToolDecl::Query(query_decl) = declaration.clone() else {
        unreachable!()
    };
    let cached_tool = crate::tools::builtin::defra_query::BoundedQueryTool::new(
        sibling_writer.clone(),
        query_decl,
    )
    .with_actor(sibling_did.clone())
    .declared_by(Some("fixture/surface".into()))
    .with_surface_owner(runtime.did().to_owned());
    let write_decl = crate::documents::WriteToolDecl {
        tool_name: "write_fixture_entry".into(),
        collection: "FixtureEntry".into(),
        description: "Write an entry".into(),
        fields: vec![crate::documents::WriteToolField {
            name: "name".into(),
            required: true,
            fill: None,
        }],
        output_obligation: None,
        notification: None,
    };
    let mailbox_decl = crate::session::mailbox::canonical_mailbox_write_decl();
    let surface = json!({"surface_id":"fixture/surface", "node_did":runtime.did(),
        "entries":[declaration.clone(), crate::documents::SurfaceToolDecl::Create(write_decl.clone()),
            crate::documents::SurfaceToolDecl::Create(mailbox_decl.clone())]});
    let sibling_tools = format!("{}:tools", sibling.agent_did);
    let tools_with_surface = json!({"node_did":runtime.did(), "tools_id":sibling_tools,
        "datastore":{"datastore_tool_surface_ids":["fixture/surface"]},
        "integrations":{"plugins":[{"plugin":"acme/hidden-plugin"}]}});
    let initial_surface_plan = DesiredStateApplyPlan::new(vec![
        DesiredStateApplyDocument {
            collection: Collection::DatastoreToolSurface,
            add: surface.clone(),
            update: surface,
        },
        DesiredStateApplyDocument {
            collection: Collection::Tools,
            add: tools_with_surface.clone(),
            update: tools_with_surface,
        },
    ])?;
    user_access
        .transact_write("test.surface.select", |txn| {
            let plan = &initial_surface_plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    let sibling_tools_query = format!(
        "{{ Tools(filter: {{tools_id: {{_eq: \"{}\"}}}}) {{ _docID integrations }} }}",
        crate::store::graphql::escape_graphql_string(&sibling_tools),
    );
    assert!(
        creator_access.execute(&sibling_tools_query).await?["data"]["Tools"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        user_access.execute(&sibling_tools_query).await?["data"]["Tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let retirement = crate::pack::ensure_plugins_unreferenced(
        &creator_access,
        runtime.did(),
        &std::collections::BTreeSet::from(["acme/hidden-plugin".to_owned()]),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        retirement.downcast_ref::<crate::pack::PackRemovalError>(),
        Some(crate::pack::PackRemovalError::ReferenceInventoryUnavailable)
    ));
    let surface_row = user_access
        .execute(
            "{ DatastoreToolSurface(filter: {surface_id: {_eq: \"fixture/surface\"}}) { _docID } }",
        )
        .await?;
    let surface_doc = surface_row["data"]["DatastoreToolSurface"][0]["_docID"]
        .as_str()
        .context("selected surface missing")?;
    user_access
        .add_actor_relationship("DatastoreToolSurface", surface_doc, "reader", &sibling_did)
        .await?;
    let surface_case = |name: &str| {
        modeled
            .configuration_datastore_tool_use_cases
            .iter()
            .find(|case| case["name"] == name)
            .with_context(|| format!("missing modeled surface case {name}"))
    };
    assert_eq!(
        cached_surface_usable(&sibling_writer, runtime.did(), &declaration).await,
        surface_case("allowed")?["expected"].as_bool().unwrap()
    );
    use crate::inference::llm::tool::Tool as _;
    let cached_write = crate::tools::builtin::defra_write::BoundedWriteTool::new(
        sibling_writer.clone(),
        write_decl.clone(),
    )
    .declared_by(Some("fixture/surface".into()))
    .with_surface_owner(runtime.did().to_owned());
    let written = cached_write
        .call(serde_json::from_value(
            json!({"name":"selected-agent-write"}),
        )?)
        .await
        .map_err(|error| anyhow::anyhow!("cached write failed: {error}"))?;
    let write_case = |name: &str| {
        modeled
            .configuration_datastore_write_cases
            .iter()
            .find(|case| case["name"] == name)
            .with_context(|| format!("missing modeled write case {name}"))
    };
    assert!(write_case("committed")?["expected"].as_bool().unwrap());
    assert!(written.contains("selected-agent-write"));
    let before_failed_write = sibling_writer
        .execute("{ FixtureEntry { _docID name } }")
        .await?;
    let (failed_write, mutations) =
        crate::store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            cached_write.call(serde_json::from_value(json!({"name":"rolled-back-write"}))?),
        )
        .await;
    assert_eq!(mutations, 1);
    assert_eq!(
        failed_write.is_ok(),
        write_case("mutation-failed")?["expected"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        sibling_writer
            .execute("{ FixtureEntry { _docID name } }")
            .await?,
        before_failed_write
    );
    let written: Value = serde_json::from_str(&written)?;
    let written_doc = written["document_id"]
        .as_str()
        .context("cached write document ID missing")?;
    let write_signer = crate::store::identity::commit_signer_identity_for_did(&sibling.agent_did)?;
    let history = sibling_writer
        .execute(&format!(
        "{{ _commits(docID: \"{}\") {{ cid height fieldName signature {{ identity type }} }} }}",
        crate::store::graphql::escape_graphql_string(written_doc),
    ))
        .await?;
    let commits =
        crate::store::graphql::composite_commits_at(history["data"].get("_commits"), "_commits")?;
    let genesis = commits
        .iter()
        .min_by_key(|commit| commit.height)
        .context("cached write genesis missing")?;
    assert_eq!(
        genesis
            .signature
            .as_ref()
            .map(|signature| signature.identity.as_str()),
        Some(write_signer.as_str())
    );
    let found = cached_tool
        .call(crate::tools::builtin::defra_query::bounded::BoundedQueryParams(Default::default()))
        .await
        .map_err(|error| anyhow::anyhow!("cached query failed: {error}"))?;
    assert!(found.contains("selected-agent-write"));
    let (_mailbox_owner, mailbox_context) = claimed_client_tool_context(
        &node,
        runtime.clone(),
        &user,
        &user_access,
        sibling_signer.as_ref(),
        "surface-mailbox-request",
    )
    .await?;
    let cached_mailbox = crate::session::mailbox::MailboxCreateTool::new(
        node.clone(),
        mailbox_decl
            .notification
            .clone()
            .context("mailbox policy missing")?,
    )
    .with_access(sibling_writer.clone())
    .with_surface_source(crate::documents::DatastoreToolSource {
        owner: runtime.did().to_owned(),
        surface_id: "fixture/surface".into(),
        declaration: crate::documents::SurfaceToolDecl::Create(mailbox_decl.clone()),
    });
    let mailbox = cached_mailbox
        .call_with_request(
            crate::session::mailbox::MailboxContentArgs {
                title: "Selected Agent notice".into(),
                summary: None,
                payload: None,
                question: None,
            },
            &mailbox_context,
        )
        .await
    .map_err(|error| anyhow::anyhow!("cached mailbox failed: {error}"))?;
    assert!(mailbox.contains("Selected Agent notice"));
    let mailbox: Value = serde_json::from_str(&mailbox)?;
    let mailbox_doc = mailbox["item"]["_docID"]
        .as_str()
        .context("cached mailbox document ID missing")?;
    let history = sibling_writer
        .execute(&format!(
        "{{ _commits(docID: \"{}\") {{ cid height fieldName signature {{ identity type }} }} }}",
        crate::store::graphql::escape_graphql_string(mailbox_doc),
    ))
        .await?;
    let commits =
        crate::store::graphql::composite_commits_at(history["data"].get("_commits"), "_commits")?;
    let genesis = commits
        .iter()
        .min_by_key(|commit| commit.height)
        .context("cached mailbox genesis missing")?;
    assert_eq!(
        genesis
            .signature
            .as_ref()
            .map(|signature| signature.identity.as_str()),
        Some(write_signer.as_str())
    );
    let (_rollback_owner, rollback_context) = claimed_client_tool_context(
        &node,
        runtime.clone(),
        &user,
        &user_access,
        sibling_signer.as_ref(),
        "surface-mailbox-rollback",
    )
    .await?;
    let before_failed_mailbox = sibling_writer
        .execute("{ MailboxItem { _docID title } }")
        .await?;
    let (failed_mailbox, mutations) =
        crate::store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            cached_mailbox.call_with_request(
                crate::session::mailbox::MailboxContentArgs {
                    title: "Rolled back notice".into(),
                    summary: None,
                    payload: None,
                    question: None,
                },
                &rollback_context,
            ),
        )
        .await;
    assert_eq!(mutations, 1);
    assert_eq!(
        failed_mailbox.is_ok(),
        write_case("mutation-failed")?["expected"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(
        sibling_writer
            .execute("{ MailboxItem { _docID title } }")
            .await?,
        before_failed_mailbox
    );
    let mut changed_declaration = declaration.clone();
    if let crate::documents::SurfaceToolDecl::Query(query) = &mut changed_declaration {
        query.fields = vec!["changed".into()];
    }
    let changed_surface = json!({"surface_id":"fixture/surface", "node_did":runtime.did(),
        "entries":[changed_declaration, crate::documents::SurfaceToolDecl::Create(write_decl.clone()),
            crate::documents::SurfaceToolDecl::Create(mailbox_decl.clone())]});
    let changed_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::DatastoreToolSurface,
        add: changed_surface.clone(),
        update: changed_surface,
    }])?;
    user_access
        .transact_write("test.surface.change", |txn| {
            let plan = &changed_plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    assert_eq!(
        cached_surface_usable(&sibling_writer, runtime.did(), &declaration).await,
        surface_case("changed")?["expected"].as_bool().unwrap()
    );
    user_access
        .transact_write("test.surface.restore", |txn| {
            let plan = &initial_surface_plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    let deselected_tools = json!({"node_did":runtime.did(), "tools_id":sibling_tools});
    let deselect_plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Tools,
        add: deselected_tools.clone(),
        update: deselected_tools,
    }])?;
    user_access
        .transact_write("test.surface.deselect", |txn| {
            let plan = &deselect_plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    assert_eq!(sibling_writer.execute("{ DatastoreToolSurface(filter: {surface_id: {_eq: \"fixture/surface\"}}) { _docID } }").await?["data"]["DatastoreToolSurface"].as_array().unwrap().len(), 1);
    assert_eq!(
        cached_surface_usable(&sibling_writer, runtime.did(), &declaration).await,
        surface_case("deselected")?["expected"].as_bool().unwrap()
    );
    user_access
        .delete_actor_relationship("DatastoreToolSurface", surface_doc, "reader", &sibling_did)
        .await?;
    user_access
        .transact_write("test.surface.reselect", |txn| {
            let plan = &initial_surface_plan;
            Box::pin(async move {
                crate::documents::apply_desired_state_plan(txn, plan).await
            })
        })
        .await?;
    assert!(cached_surface_usable(&sibling_writer, runtime.did(), &declaration).await);
    let tools_rows = user_access
        .execute(&format!(
            "{{ Tools(filter: {{tools_id: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
            crate::store::graphql::escape_graphql_string(&sibling_tools),
        ))
        .await?;
    let sibling_tools_doc = tools_rows["data"]["Tools"][0]["_docID"]
        .as_str()
        .context("Tools missing")?;
    let read_started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let paused_once = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut in_flight = tokio::spawn({
        let access = sibling_writer.clone();
        let owner = runtime.did().to_owned();
        let declaration = declaration.clone();
        let read_started = read_started.clone();
        let resume = resume.clone();
        let paused_once = paused_once.clone();
        async move {
            access.transact_write("test.surface.revoke_race", |txn| {
                let read_started = read_started.clone();
                let resume = resume.clone();
                let paused_once = paused_once.clone();
                let owner = &owner;
                let declaration = &declaration;
                Box::pin(async move {
                    crate::documents::revalidate_datastore_tool_in_txn(txn, owner, "fixture/surface", declaration).await?;
                    if !paused_once.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        read_started.notify_one();
                        resume.notified().await;
                    }
                    txn.execute("mutation { add_FixtureEntry(input: {name: \"raced-revocation\"}) { _docID } }").await?;
                    Ok::<_, anyhow::Error>(())
                })
            }).await
        }
    });
    let started = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            _ = read_started.notified() => Ok(()),
            result = &mut in_flight => anyhow::bail!("cached tool transaction exited before its source read: {result:?}"),
        }
    }).await;
    match started {
        Ok(result) => result?,
        Err(_) => {
            resume.notify_one();
            in_flight.abort();
            let _ = in_flight.await;
            anyhow::bail!("cached tool source read did not start");
        }
    }
    let revocation = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        user_access.transact_write("test.surface.revoke", |txn| {
            Box::pin(async move {
                txn.delete_entity_set_relationship(
                    "DatastoreToolSurface",
                    surface_doc,
                    "selected_by",
                    "configuration",
                    sibling_tools_doc,
                    "selected_by",
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        }),
    )
    .await;
    resume.notify_one();
    let raced = match tokio::time::timeout(std::time::Duration::from_secs(10), &mut in_flight).await
    {
        Ok(result) => result?,
        Err(_) => {
            in_flight.abort();
            let _ = in_flight.await;
            anyhow::bail!("cached tool transaction did not finish");
        }
    };
    revocation.context("surface revocation was blocked by cached tool transaction")??;
    assert_eq!(
        raced.is_ok(),
        surface_case("revoked")?["expected"].as_bool().unwrap(),
        "revocation did not fence a source observation and write: {raced:?}"
    );
    let raced_rows = sibling_writer
        .execute("{ FixtureEntry(filter: {name: {_eq: \"raced-revocation\"}}) { _docID } }")
        .await?;
    assert!(raced_rows["data"]["FixtureEntry"]
        .as_array()
        .context("raced entries missing")?
        .is_empty());
    assert_eq!(
        cached_surface_usable(&sibling_writer, runtime.did(), &declaration).await,
        surface_case("revoked")?["expected"].as_bool().unwrap()
    );
    let error = crate::inference::llm::tool::Tool::call(
        &cached_tool,
        crate::tools::builtin::defra_query::bounded::BoundedQueryParams(Default::default()),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("missing same-agent DatastoreToolSurface"),
        "{error}"
    );
    let before_entries = sibling_writer
        .execute("{ FixtureEntry { _docID name } }")
        .await?;
    let before_mailbox = sibling_writer
        .execute("{ MailboxItem { _docID title } }")
        .await?;
    let denied = cached_write
        .call(serde_json::from_value(json!({"name":"denied"}))?)
        .await
        .unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("missing same-agent DatastoreToolSurface"),
        "{denied}"
    );
    let denied = cached_mailbox
        .call_with_request(
            crate::session::mailbox::MailboxContentArgs {
                title: "Denied notice".into(),
                summary: None,
                payload: None,
                question: None,
            },
            &mailbox_context,
        )
        .await
        .unwrap_err();
    assert!(
        denied
            .to_string()
            .contains("missing same-agent DatastoreToolSurface"),
        "{denied}"
    );
    assert_eq!(
        sibling_writer
            .execute("{ FixtureEntry { _docID name } }")
            .await?,
        before_entries
    );
    assert_eq!(
        sibling_writer
            .execute("{ MailboxItem { _docID title } }")
            .await?,
        before_mailbox
    );
    let before_denied = user_access
        .execute(&format!(
            "{{ PackInstallation(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID version }} }}",
            crate::store::graphql::escape_graphql_string(record_doc),
        ))
        .await?;
    let denied = sibling_writer.write("test.pack.foreign_update", &format!(
        "mutation {{ update_PackInstallation(docID: \"{}\", input: {{version: \"foreign\"}}) {{ _docID }} }}",
        crate::store::graphql::escape_graphql_string(record_doc),
    )).await;
    if let Ok(response) = &denied {
        assert!(response["data"]["update_PackInstallation"]
            .as_array()
            .is_none_or(Vec::is_empty));
    }
    let after_denied = user_access
        .execute(&format!(
            "{{ PackInstallation(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID version }} }}",
            crate::store::graphql::escape_graphql_string(record_doc),
        ))
        .await?;
    assert_eq!(after_denied, before_denied);
    let removal_case = |name: &str| {
        modeled
            .configuration_pack_removal_cases
            .iter()
            .find(|case| case["name"] == name)
            .with_context(|| format!("missing modeled pack removal case {name}"))
    };
    for (actor, name) in [
        (&agent_did, "agent-owner"),
        (&user_did, "user-parent"),
        (&runtime_did, "runtime-reader"),
    ] {
        assert_pack_policy_observation(
            &admin,
            removal_case(name)?,
            actor,
            record_doc,
            "delete",
            "expected",
        )
        .await?;
    }
    // The Agent installed a plugin-only pack, which `remove_pack` refuses for
    // every actor before any write: its plugins are removed through plugin
    // remove. The User's attempt leaves the record unchanged.
    let user_delete = crate::pack::remove_pack(
        &user_access,
        runtime.did(),
        "fixture/native_echo_pack",
        crate::pack::DriftPolicy::Refuse,
    )
    .await
    .unwrap_err();
    assert!(
        format!("{user_delete:#}").contains("only its plugins are"),
        "{user_delete:#}"
    );
    assert_eq!(
        user_access
            .execute(&format!(
                "{{ PackInstallation(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID version }} }}",
                crate::store::graphql::escape_graphql_string(record_doc),
            ))
            .await?,
        before_denied
    );
    admin
        .grant_node_permission(&sibling_did, NodePermission::DocumentDelete)
        .await?;
    admin
        .grant_node_permission(&agent_did, NodePermission::DacRelationDelete)
        .await?;
    creator_access
        .add_actor_relationship("PackInstallation", record_doc, "writer", &sibling_did)
        .await?;
    assert_pack_policy_observation(
        &admin,
        removal_case("agent-writer")?,
        &sibling_did,
        record_doc,
        "delete",
        "expected",
    )
    .await?;
    let attempted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let native_denied: Result<()> = sibling_writer.transact_write("test.pack.writer_delete", |txn| {
        let attempted = attempted.clone();
        Box::pin(async move {
            attempted.store(true, std::sync::atomic::Ordering::Release);
            let response = txn.execute(&format!(
                "mutation {{ delete_PackInstallation(filter: {{_docID: {{_eq: \"{}\"}}}}) {{_docID}} }}",
                crate::store::graphql::escape_graphql_string(record_doc),
            )).await?;
            let deleted = response["data"]["delete_PackInstallation"].as_array().context("delete response missing")?;
            anyhow::ensure!(deleted.len() == 1 && deleted[0]["_docID"] == record_doc, "delete was not confirmed");
            Ok(())
        })
    }).await;
    assert!(attempted.load(std::sync::atomic::Ordering::Acquire));
    assert!(native_denied.is_err());
    let denied_removal = crate::pack::remove_pack(
        &sibling_writer,
        runtime.did(),
        "fixture/native_echo_pack",
        crate::pack::DriftPolicy::Refuse,
    )
    .await;
    assert!(denied_removal.is_err());
    assert_eq!(
        user_access
            .execute(&format!(
                "{{ PackInstallation(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID version }} }}",
                crate::store::graphql::escape_graphql_string(record_doc),
            ))
            .await?,
        before_denied
    );
    creator_access
        .delete_actor_relationship("PackInstallation", record_doc, "writer", &sibling_did)
        .await?;

    let user_home = tempfile::tempdir()?;
    let mut user_pack =
        crate::pack::PackIdentity::new(distribution.manifest(), distribution.digest(), vec![]);
    user_pack.coordinate = "fixture/user_pack".into();
    user_pack
        .agent_bindings
        .insert("worker".into(), creator.clone());
    crate::pack::record_plugin_store_change(
        &user_access,
        runtime.did(),
        user_home.path(),
        &user_pack.coordinate,
        Some(&user_pack),
    )
    .await?;
    let user_record = user_access.execute("{ PackInstallation(filter: {coordinate: {_eq: \"fixture/user_pack\"}}) { _docID agent_bindings version } }").await?;
    let user_doc = user_record["data"]["PackInstallation"][0]["_docID"]
        .as_str()
        .context("user installation record missing")?
        .to_owned();
    assert_pack_policy_case(
        &admin,
        model_case("user-created")?,
        &user_did,
        &user_did,
        &runtime_did,
        &sibling_did,
        &user_doc,
    )
    .await?;
    let original_bindings = user_record["data"]["PackInstallation"][0]["agent_bindings"].clone();
    assert!(!original_bindings.is_null());
    user_pack.version = "2.0.0".into();
    crate::pack::record_plugin_store_change(
        &user_access,
        runtime.did(),
        user_home.path(),
        &user_pack.coordinate,
        Some(&user_pack),
    )
    .await?;
    let upgraded = user_access.execute("{ PackInstallation(filter: {coordinate: {_eq: \"fixture/user_pack\"}}) { _docID agent_bindings version } }").await?;
    assert_eq!(upgraded["data"]["PackInstallation"][0]["_docID"], user_doc);
    assert_eq!(
        upgraded["data"]["PackInstallation"][0]["agent_bindings"],
        original_bindings
    );
    assert_eq!(upgraded["data"]["PackInstallation"][0]["version"], "2.0.0");
    let user_commits =
        crate::store::graphql::composite_commits(&node, &user_doc, "test user pack history")
            .await?;
    let user_signer = crate::store::identity::commit_signer_identity_for_did(user.did())?;
    for commit in [
        user_commits.iter().min_by_key(|commit| commit.height),
        user_commits.iter().max_by_key(|commit| commit.height),
    ] {
        assert_eq!(
            commit
                .context("user pack commit missing")?
                .signature
                .as_ref()
                .map(|signature| signature.identity.as_str()),
            Some(user_signer.as_str())
        );
    }

    let node_doc = core
        .node_binding()
        .context("Node binding missing")?
        .doc_id
        .clone();
    creator_access
        .delete_resource_relationship("PackInstallation", record_doc, "parent", "nodes", &node_doc)
        .await?;
    crate::pack::record_plugin_store_change_as(
        &creator_access,
        runtime.did(),
        Some(agent_did.clone()),
        plugin_home.path(),
        "fixture/native_echo_pack",
        None,
    )
    .await?;
    // The record's parent edge is gone, so its history is read by its owner.
    let agent_history = creator_access
        .execute(&format!(
            "{{ _commits(docID: \"{}\") {{ cid height fieldName signature {{ identity type }} }} }}",
            crate::store::graphql::escape_graphql_string(record_doc),
        ))
        .await?;
    let agent_commits = crate::store::graphql::composite_commits_at(
        agent_history["data"].get("_commits"),
        "_commits",
    )?;
    let record_genesis = agent_commits
        .iter()
        .min_by_key(|commit| commit.height)
        .context("agent pack genesis missing")?;
    let latest = agent_commits
        .iter()
        .max_by_key(|commit| commit.height)
        .context("agent pack update missing")?;
    assert!(latest.height > record_genesis.height);
    assert_eq!(
        latest
            .signature
            .as_ref()
            .map(|signature| signature.identity.as_str()),
        Some(creator_signer.as_str())
    );
    assert_pack_policy_case(
        &admin,
        model_case("agent-parent-revoked")?,
        &agent_did,
        &user_did,
        &runtime_did,
        &sibling_did,
        record_doc,
    )
    .await?;
    let claimed_fresh =
        crate::pack::PackIdentity::new(distribution.manifest(), distribution.digest(), vec![]);
    assert!(crate::pack::record_plugin_store_change(
        &user_access,
        runtime.did(),
        user_home.path(),
        &claimed_fresh.coordinate,
        Some(&claimed_fresh),
    )
    .await
    .is_err());
    assert_pack_policy_case(
        &admin,
        model_case("agent-parent-revoked-claimed-fresh")?,
        &agent_did,
        &user_did,
        &runtime_did,
        &sibling_did,
        record_doc,
    )
    .await?;

    let staged = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let aborted: Result<()> = creator_access.transact_write("test.pack.abort", |txn| {
        let staged = staged.clone();
        let node_doc = node_doc.clone();
        let owner = runtime.did().to_owned();
        Box::pin(async move {
            let response = txn.execute(&format!("mutation {{ create_PackInstallation(input: {{node_did: \"{}\", coordinate: \"fixture/aborted\"}}) {{ _docID }} }}", crate::store::graphql::escape_graphql_string(&owner))).await?;
            let doc = crate::store::graphql::created_doc_id(&response, "PackInstallation")?;
            txn.add_resource_relationship("PackInstallation", &doc, "parent", "nodes", &node_doc).await?;
            staged.store(true, std::sync::atomic::Ordering::Release);
            anyhow::bail!("abort modeled publication")
        })
    }).await;
    assert!(aborted.is_err());
    assert!(staged.load(std::sync::atomic::Ordering::Acquire));
    let abort_case = model_case("aborted")?;
    assert_eq!(abort_case["installer_read"], false);
    assert_eq!(abort_case["user_read"], false);
    for access in [&creator_access, &user_access] {
        assert!(access
            .execute(
                "{ PackInstallation(filter: {coordinate: {_eq: \"fixture/aborted\"}}) { _docID } }"
            )
            .await?["data"]["PackInstallation"]
            .as_array()
            .context("aborted pack query missing")?
            .is_empty());
    }
    let direct = creator_access.write("test.pack.direct_create", &format!(
        "mutation {{ create_PackInstallation(input: {{node_did: \"{}\", coordinate: \"fixture/direct-orphan\"}}) {{_docID}} }}",
        crate::store::graphql::escape_graphql_string(runtime.did()),
    )).await?;
    let direct_doc = crate::store::graphql::created_doc_id(&direct, "PackInstallation")?;
    assert_pack_policy_case(
        &admin,
        model_case("direct-orphan")?,
        &agent_did,
        &user_did,
        &runtime_did,
        &sibling_did,
        &direct_doc,
    )
    .await?;
    assert_eq!(removal_case("agent-owner")?["expected"], true);
    // A plugin-only pack is retired through plugin removal: the local store
    // releases its plugins, then the Agent records the disappearance.
    let local_home = plugin_home.path().to_owned();
    let local = tokio::task::spawn_blocking(move || {
        crate::pack::plugin_lifecycle::remove_local(
            &local_home,
            "fixture/native_echo_pack",
            |_| Ok(()),
            |()| Ok(()),
        )
    })
    .await??;
    assert!(!local.plugins.is_empty());
    crate::pack::record_plugin_store_change_as(
        &creator_access,
        runtime.did(),
        Some(agent_did.clone()),
        plugin_home.path(),
        "fixture/native_echo_pack",
        None,
    )
    .await?;
    assert!(
        crate::pack::read_home_install(plugin_home.path(), "fixture/native_echo_pack")?.is_none()
    );
    // Without a complete reference inventory the disappearance notice keeps
    // the installation evidence, refreshed by the Agent (`retainPluginStoreRecord`).
    let retain = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_plugin_reference_cases()
            .iter()
            .find(|case| case["root_ships"].as_array().is_some_and(|ships| !ships.is_empty()))
            .and_then(|case| case["retain_notification_record"].as_bool())
    })
    .await?
    .context("modeled plugin retirement case missing")?;
    let retained = creator_access
        .execute("{ PackInstallation(filter: {coordinate: {_eq: \"fixture/native_echo_pack\"}}) { _docID } }")
        .await?;
    let retained = retained["data"]["PackInstallation"]
        .as_array()
        .context("removed pack query missing")?;
    assert_eq!(!retained.is_empty(), retain);
    if retain {
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0]["_docID"], record_doc);
        let history = creator_access
            .execute(&format!(
                "{{ _commits(docID: \"{}\") {{ cid height fieldName signature {{ identity type }} }} }}",
                crate::store::graphql::escape_graphql_string(record_doc),
            ))
            .await?;
        let notice = crate::store::graphql::composite_commits_at(
            history["data"].get("_commits"),
            "_commits",
        )?
        .into_iter()
        .max_by_key(|commit| commit.height)
        .context("plugin removal notice missing")?;
        assert!(notice.height > latest.height);
        assert_eq!(
            notice
                .signature
                .as_ref()
                .map(|signature| signature.identity.as_str()),
            Some(creator_signer.as_str())
        );
    }
    let archive =tokio::task::spawn_blocking(|| -> Result<crate::pack::archive::PackArchive> {
        let directory = tempfile::tempdir()?;
        let manifest = json!({"manifest_version":1,"name":"native_child","namespace":"fixture","version":"1.0.0",
            "description":"Agent provisioning fixture","kind":"documents","authors":["Fixture"],
            "assets":["README.md","config.json"],"config":"config.json",
            "inference_slots":[{"name":"model","description":"Model","agents":["worker"]}]});
        let config = json!({"node":{},"agents":[{"agent_slot":"worker","display_name":"Pack child",
            "inference_profile_id":"gents:inference-slot:model"}]});
        std::fs::write(directory.path().join("manifest.json"), serde_json::to_vec(&manifest)?)?;
        std::fs::write(directory.path().join("config.json"), serde_json::to_vec(&config)?)?;
        std::fs::write(directory.path().join("README.md"), "# Fixture\n\n## Usage\nFixture")?;
        let (bytes, _) = crate::pack::archive::pack_dir(directory.path())?;
        crate::pack::archive::PackArchive::from_bytes(&bytes)
    }).await??;
    let distribution =
        super::pack_installer::ConfigPackDistribution(crate::pack::resolve::ResolvedNamedPack {
            archive,
            from: crate::pack::resolve::ResolvedFrom::Store,
        });
    let installer = super::pack_installer::PackInstaller {
        core: core.clone(),
        node: node.clone(),
        actor: Some(signer.clone()),
        home: None,
    };
    let args = super::pack_installer::PackInstallParams {
        package: "fixture/native_child".into(),
        variables: Default::default(),
        inference_slots: std::collections::BTreeMap::from([("model".into(), "profile".into())]),
        expected_digest: None,
    };
    let pack: Value = serde_json::from_str(
        &installer
            .documents("install", &args, &distribution, true)
            .await?,
    )?;
    let pack_child = pack["install"]["agents"]["worker"]
        .as_str()
        .context("pack child missing")?;
    assert_ne!(pack_child, child);
    let pack_query = format!(
        r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID creation_nonce }} }}"#,
        crate::store::graphql::escape_graphql_string(pack_child)
    );
    let pack_row = creator_access.execute(&pack_query).await?;
    assert_eq!(pack_row["data"]["Agent"].as_array().unwrap().len(), 1);
    assert_eq!(
        user_access.execute(&pack_query).await?["data"]["Agent"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        sibling_access.execute(&pack_query).await?["data"]["Agent"],
        json!([])
    );
    let binding = gents_protocol::agent_identity::AgentIdentityBinding {
        agent_did: pack_child.to_owned(),
        creation_nonce: pack_row["data"]["Agent"][0]["creation_nonce"]
            .as_str()
            .context("pack nonce missing")?
            .to_owned(),
    };
    let pack_signer = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let pack_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(pack_child)?,
        pack_signer.as_ref(),
    )?;
    assert_eq!(
        pack_access.execute(&pack_query).await?["data"]["Agent"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let creator_did = identity::Did::new(signer.did())?;
    // The creator holds `add-dac-relation` and `update-document` through its
    // projected baseline as well as any direct grant, so revoking the direct
    // edges leaves creation admitted (`GrantSets.agentEffective`).
    for permission in [
        NodePermission::DacRelationAdd,
        NodePermission::DocumentUpdate,
    ] {
        admin
            .revoke_node_permission(&creator_did, permission)
            .await?;
    }
    agent_management::model_agent_mutate(&core, signer.as_ref(), &create).await?;
    // Publishing an Agent projects its baseline, which needs the policy
    // delegate's `add-nac-relation`; without it the create rolls back whole
    // (`GrantSets.baseline_agent_cannot_publish_agent`).
    let before = creator_access
        .execute("{ Agent { _docID } AgentContext { _docID } }")
        .await?;
    admin
        .revoke_node_permission(&creator_did, NodePermission::NacRelationAdd)
        .await?;
    assert!(
        agent_management::model_agent_mutate(&core, signer.as_ref(), &create)
            .await
            .is_err()
    );
    let after = creator_access
        .execute("{ Agent { _docID } AgentContext { _docID } }")
        .await?;
    for collection in ["Agent", "AgentContext"] {
        let ids = |response: &Value| -> std::collections::BTreeSet<String> {
            response["data"][collection]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["_docID"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            ids(&before),
            ids(&after),
            "a refused baseline must roll back {collection} creation"
        );
    }
    node.shutdown().await?;
    Ok(())
}
