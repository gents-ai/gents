use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::documents::apply_desired_state_plan;
use crate::inference::resolved::{
    ResolvedConfiguration, SelectedConfiguration, SelectedConfigurationDocument,
};
use crate::store::config_client::{
    ConfigAccess, ConfigClientError, DesiredStateApplyDocument, DesiredStateApplyPlan,
    GraphqlEndpoint,
};
use crate::store::identity::NodeIdentity;
use crate::Collection;

use super::shared::{lookup_service, MetaToolContext};
use crate::inference::llm::tool::Tool;

async fn invoke(tool: &super::CallToolTool, service: &str, name: &str) -> Result<String> {
    tool.call(super::CallToolArgs {
        service_id: service.into(),
        tool_name: name.into(),
        arguments: json!({}),
    })
    .await
    .map_err(anyhow::Error::new)
}

fn plan(documents: Vec<(Collection, Value)>) -> Result<DesiredStateApplyPlan> {
    DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
}

async fn apply(access: &ConfigAccess, plan: &DesiredStateApplyPlan) -> Result<()> {
    access
        .transact_write("test.mcp.policy.apply", |txn| {
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
}

fn selected(service: &str) -> crate::documents::RemoteTools {
    crate::documents::RemoteTools {
        services: vec![crate::documents::RemoteServiceTools {
            mcp_service_id: service.to_owned(),
            tool_names: vec!["selected".to_owned()],
            ..Default::default()
        }],
    }
}

async fn doc(access: &ConfigAccess, name: &str) -> Result<String> {
    let response = access
        .execute(&format!("{{ {name} {{ _docID }} }}"))
        .await?;
    let rows = response["data"][name].as_array().context("missing rows")?;
    let [row] = rows.as_slice() else {
        anyhow::bail!("expected exactly one {name}")
    };
    Ok(row["_docID"]
        .as_str()
        .context("missing document ID")?
        .to_owned())
}

async fn assert_genesis_signer(
    node: &defra_node::Node,
    access: &ConfigAccess,
    doc_id: &str,
    signer: &str,
) -> Result<()> {
    let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
    let history = access
        .execute(&format!(
            "{{ _commits(docID: \"{doc_id}\") {{cid height fieldName}} }}"
        ))
        .await?;
    let cid = history["data"]["_commits"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .filter(|row| row["fieldName"] == "_C")
                .min_by_key(|row| row["height"].as_i64().unwrap_or_default())
        })
        .and_then(|row| row["cid"].as_str())
        .context("service genesis is unavailable")?;
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(signer))
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature)
            .map_err(anyhow::Error::msg)?,
        signer
    );
    Ok(())
}

/// The selected-by edges and canonical policy are supplied by the configuration
/// selection owner. Client publication uses an actual held user key over HTTP;
/// Agent writes use the retained managed signer from the Node root.
#[tokio::test]
async fn held_client_and_agent_mcp_selection_isolation_rebind_and_rollback() -> Result<()> {
    let modeled = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_identity_household_cases()
            .iter()
            .find(|case| case.name == "configuration-explicit-grants")
            .expect("MCP use model")
            .mcp_service_use_cases
            .clone()
    })
    .await?;
    let expected = |field: &str, value: bool| {
        modeled
            .iter()
            .find(|case| {
                case[field] == value
                    && [
                        "same_actor",
                        "same_chain",
                        "same_selection",
                        "selected",
                        "service_visible",
                        "enabled",
                    ]
                    .iter()
                    .filter(|name| **name != field)
                    .all(|name| case[*name] == true)
            })
            .and_then(|case| case["allowed"].as_bool())
            .expect("modeled MCP use case")
    };
    let directory = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let sibling = crate::KeyIdentity::load_or_create(directory.path().join("sibling"), None)?;
    let outsider = crate::KeyIdentity::load_or_create(directory.path().join("outsider"), None)?;
    let binding = crate::store::identity::provision_agent_identity(&runtime)?;
    let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    crate::ensure_runtime_schemas(node.as_ref()).await?;
    let owner = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    // The user owns node ACP here, so the runtime that observes its bound Node
    // holds node read access like any other granted actor.
    for actor in [runtime.did(), agent.did(), sibling.did(), outsider.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&owner, permission.as_str(), actor)
                .await?;
        }
    }
    node.add_nac_actor_relationship_as(
        &owner,
        acp::nac::NodePermission::DacRelationAdd.as_str(),
        agent.did(),
    )
    .await?;
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    let node_response = access
        .transact_write("test.mcp.policy.node", |txn| {
            Box::pin(async {
                txn.execute_desired_state_mutation(
            "mutation($input: NodeMutationInputArg!) { create_Node(input:$input) {_docID} }",
            &json!({"input":{"node_did": runtime.did()}}), Collection::Node, true,
        ).await
            })
        })
        .await?;
    let node_doc = gents_protocol::graphql::extract_mutation_doc_id(&node_response, "Node")?;
    access
        .add_actor_relationship(
            "Node",
            &node_doc,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let service = |id: &str, hostname: &str| json!({"node_did":runtime.did(),"service_id":id,"hostname":hostname,"mcp_port":8080,"enabled":true});
    apply(
        &access,
        &plan(vec![
            (Collection::ToolServiceRegistry, service("one", "one")),
            (Collection::ToolServiceRegistry, service("two", "two")),
        ])?,
    )
    .await?;
    let remote_one = selected("one");
    let tools = |remote: &crate::documents::RemoteTools| json!({"node_did":runtime.did(),"tools_id":"tools","remote":remote});
    apply(&access, &plan(vec![
        (Collection::Agent, json!({"node_did":runtime.did(),"agent_did":binding.agent_did,"creation_nonce":binding.creation_nonce,"context_id":"context","inference_profile_id":"profile"})),
        (Collection::AgentContext, json!({"node_did":runtime.did(),"context_id":"context","tools_id":"tools"})),
        (Collection::Tools, tools(&remote_one)),
        (Collection::InferenceProfile, json!({"node_did":runtime.did(),"profile_id":"profile","backend_id":"backend","model_name":"test-model"})),
        (Collection::InferenceBackend, json!({"node_did":runtime.did(),"backend_id":"backend","name":"test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
    ])?).await?;
    let agent_did = identity::Did::new(agent.did())?;
    let agent_access =
        ConfigAccess::local_managed_agent(node.clone(), agent_did.clone(), agent.as_ref())?;
    let docs = vec![
        SelectedConfigurationDocument {
            collection: Collection::Agent,
            doc_id: doc(&access, "Agent").await?,
            logical_id: None,
        },
        SelectedConfigurationDocument {
            collection: Collection::AgentContext,
            doc_id: doc(&access, "AgentContext").await?,
            logical_id: None,
        },
        SelectedConfigurationDocument {
            collection: Collection::Tools,
            doc_id: doc(&access, "Tools").await?,
            logical_id: None,
        },
    ];
    let tools_doc = docs[2].doc_id.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let call_count = calls.clone();
    let endpoints = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_endpoints = endpoints.clone();
    let pool = crate::tools::mcp::pool::McpPool::new_with_tool_handlers(
        move |_, endpoint| {
            let observed = observed_endpoints.clone();
            async move {
                observed.lock().unwrap().push(endpoint);
                Ok(rmcp::model::ListToolsResult::with_all_items(vec![
                    rmcp::model::Tool::new(
                        "selected",
                        "selected",
                        Arc::new(json!({"type":"object"}).as_object().unwrap().clone()),
                    ),
                ]))
            }
        },
        move |_| {
            let count = call_count.clone();
            async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(rmcp::model::CallToolResult::success(vec![
                    rmcp::model::Content::text("invoked"),
                ]))
            }
        },
    );
    let mut ctx = MetaToolContext {
        actor: agent_did.clone(),
        node: node.clone(),
        mcp_pool: pool.for_agent(agent.did()),
        health: crate::tools::mcp::health::ServiceHealthMap::new(),
        local_hostname: "local".into(),
        local_subnet: None,
        node_did: runtime.did().to_owned(),
        allowed_mcp_service_ids: vec!["one".into(), "two".into()],
        remote_tools: remote_one.clone(),
        configuration: ResolvedConfiguration::Documents(SelectedConfiguration::new(
            node.clone(),
            agent_did.clone(),
            docs,
        )),
    };
    assert_eq!(
        lookup_service(&ctx, "one").await?.endpoint,
        "http://one:8080"
    );
    assert!(expected("service_visible", true));
    let old_tool = super::CallToolTool::new(ctx.clone());
    assert_eq!(invoke(&old_tool, "one", "selected").await?, "invoked");
    assert!(invoke(&old_tool, "one", "unselected").await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut wrong_actor = ctx.clone();
    wrong_actor.actor = identity::Did::new(sibling.did())?;
    assert_eq!(
        lookup_service(&wrong_actor, "one").await.is_ok(),
        expected("same_actor", false)
    );
    let mut wrong_chain = ctx.clone();
    let ResolvedConfiguration::Documents(selection) = &mut wrong_chain.configuration else {
        unreachable!()
    };
    selection.documents[1].doc_id = tools_doc.clone();
    assert_eq!(
        lookup_service(&wrong_chain, "one").await.is_ok(),
        expected("same_chain", false)
    );
    let query = "{ ToolServiceRegistry { service_id } }";
    let rows = |response: &Value| {
        response["data"]["ToolServiceRegistry"]
            .as_array()
            .map_or(0, Vec::len)
    };
    assert_eq!(rows(&access.execute(query).await?), 2);
    assert_eq!(rows(&agent_access.execute(query).await?), 1);
    for actor in [sibling.did(), outsider.did()] {
        let reader = ConfigAccess::local_as(node.clone(), identity::Did::new(actor)?);
        assert_eq!(rows(&reader.execute(query).await?), 0);
    }
    let one = access
        .execute("{ ToolServiceRegistry(filter:{service_id:{_eq:\"one\"}}) {_docID} }")
        .await?;
    let one_doc = one["data"]["ToolServiceRegistry"][0]["_docID"]
        .as_str()
        .context("service missing")?
        .to_owned();
    access
        .transact_write("test.mcp.revoke_selection", |txn| {
            let (service_doc, tools_doc) = (&one_doc, &tools_doc);
            Box::pin(async move {
                txn.delete_entity_set_relationship(
                    "ToolServiceRegistry",
                    service_doc,
                    "selected_by",
                    "configuration",
                    tools_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    assert_eq!(
        lookup_service(&ctx, "one").await.is_ok(),
        expected("service_visible", false)
    );
    assert_eq!(rows(&agent_access.execute(query).await?), 0);
    assert!(invoke(&old_tool, "one", "selected").await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    access
        .transact_write("test.mcp.restore_selection", |txn| {
            let (service_doc, tools_doc) = (&one_doc, &tools_doc);
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "ToolServiceRegistry",
                    service_doc,
                    "selected_by",
                    "configuration",
                    tools_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    assert!(lookup_service(&ctx, "one").await.is_ok());
    let remote_two = selected("two");
    apply(
        &access,
        &plan(vec![(Collection::Tools, tools(&remote_two))])?,
    )
    .await?;
    assert_eq!(
        lookup_service(&ctx, "one").await.is_ok(),
        expected("same_selection", false)
    );
    assert!(invoke(&old_tool, "one", "selected").await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    ctx.remote_tools = remote_two.clone();
    assert_eq!(
        lookup_service(&ctx, "two").await?.endpoint,
        "http://two:8080"
    );
    assert_eq!(rows(&agent_access.execute(query).await?), 1);
    let current_tool = super::CallToolTool::new(ctx.clone());
    assert_eq!(invoke(&current_tool, "two", "selected").await?, "invoked");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let mut changed = service("two", "two");
    changed["hostname"] = json!("changed");
    let update = plan(vec![(Collection::ToolServiceRegistry, changed.clone())])?;
    let denied = apply(&agent_access, &update).await.unwrap_err();
    assert!(
        matches!(
            denied.downcast_ref::<ConfigClientError>(),
            Some(ConfigClientError::ConfigurationMutationNotApplied { .. })
        ),
        "{denied:?}"
    );
    let service_rows = access
        .execute("{ ToolServiceRegistry(filter:{service_id:{_eq:\"two\"}}) {_docID hostname} }")
        .await?;
    let service_doc = service_rows["data"]["ToolServiceRegistry"][0]["_docID"]
        .as_str()
        .context("service missing")?;
    assert_genesis_signer(&node, &access, service_doc, user.did()).await?;
    assert_eq!(
        service_rows["data"]["ToolServiceRegistry"][0]["hostname"],
        "two"
    );
    access
        .add_actor_relationship("ToolServiceRegistry", service_doc, "writer", &agent_did)
        .await?;
    apply(&agent_access, &update).await?;
    assert_eq!(
        lookup_service(&ctx, "two").await?.endpoint,
        "http://changed:8080"
    );
    assert_eq!(invoke(&current_tool, "two", "selected").await?, "invoked");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(endpoints
        .lock()
        .unwrap()
        .iter()
        .any(|endpoint| endpoint == "http://changed:8080"));
    access
        .delete_actor_relationship("ToolServiceRegistry", service_doc, "writer", &agent_did)
        .await?;
    let mut revoked = changed.clone();
    revoked["hostname"] = json!("revoked");
    let denied = apply(
        &agent_access,
        &plan(vec![(Collection::ToolServiceRegistry, revoked)])?,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        denied.downcast_ref::<ConfigClientError>(),
        Some(ConfigClientError::ConfigurationMutationNotApplied { .. })
    ));
    apply(
        &access,
        &plan(vec![(
            Collection::Tools,
            tools(&crate::documents::RemoteTools::default()),
        )])?,
    )
    .await?;
    assert!(lookup_service(&ctx, "two").await.is_err());
    assert!(invoke(&current_tool, "two", "selected").await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(rows(&agent_access.execute(query).await?), 0);
    let invalid = selected("missing");
    let failed = plan(vec![
        (Collection::ToolServiceRegistry, service("three", "three")),
        (Collection::Tools, tools(&invalid)),
    ])?;
    assert!(apply(&access, &failed).await.is_err());
    let check = access
        .execute("{ ToolServiceRegistry(filter:{service_id:{_eq:\"three\"}}) {_docID} }")
        .await?;
    assert_eq!(rows(&check), 0);
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    assert_eq!(
        rows(
            &runtime_access
                .execute("{ ToolServiceRegistry(filter:{service_id:{_eq:\"three\"}}) {_docID} }")
                .await?
        ),
        0
    );
    let agent_service = plan(vec![(
        Collection::ToolServiceRegistry,
        service("agent-owned", "agent"),
    )])?;
    let node_binding = crate::store::config_client::ConfigurationNodeBinding {
        node_did: identity::Did::new(runtime.did())?,
        doc_id: node_doc,
        enabled: true,
        default_agent_did: None,
    };
    agent_access
        .transact_write("test.mcp.agent_create", |txn| {
            let (plan, binding) = (&agent_service, &node_binding);
            Box::pin(async move {
                crate::documents::apply_desired_state_plan_with_node_binding(
                    txn,
                    plan,
                    Some(binding),
                )
                .await?;
                Ok::<_, anyhow::Error>(())
            })
        })
        .await?;
    let agent_query = "{ ToolServiceRegistry(filter:{service_id:{_eq:\"agent-owned\"}}) {_docID} }";
    let own = agent_access.execute(agent_query).await?;
    let own_doc = own["data"]["ToolServiceRegistry"][0]["_docID"]
        .as_str()
        .context("agent-owned service missing")?;
    assert_genesis_signer(&node, &agent_access, own_doc, agent.did()).await?;
    assert_eq!(rows(&access.execute(agent_query).await?), 1);
    let sibling_access = ConfigAccess::local_as(node.clone(), identity::Did::new(sibling.did())?);
    assert_eq!(rows(&sibling_access.execute(agent_query).await?), 0);
    node.shutdown().await?;
    Ok(())
}
