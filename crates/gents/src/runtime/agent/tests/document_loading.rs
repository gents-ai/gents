use std::sync::Arc;
use std::time::Duration;

use gents_protocol::node_readiness::AgentReadinessUnavailableReason;

use super::super::*;
use super::support::*;
use crate::documents::{Agent, AgentTargetDocument, Tools};
use crate::store::config_client::ConfigAccess;
use crate::tools::native::ToolSet;
use crate::tools::surface::ToolCeiling;

#[tokio::test]
async fn document_constructor_rejects_node_without_signing_did_before_migrations() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let identity = Arc::new(test_identity("document-constructor-unsigned-node"));

    let error = match Gents::from_default_agent_documents(
        node,
        identity,
        DocumentRuntimeOptions::default(),
    )
    .await
    {
        Ok(_) => panic!("document constructor should reject an unsigned node"),
        Err(error) => error,
    };

    assert!(error
        .to_string()
        .contains("EmbeddedNode configured with a node signing DID"));
}

#[tokio::test]
async fn from_default_agent_documents_preserves_identity_only_principal() {
    let (db, _owner_identity, _owner) = test_node("bootstrap-profile").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let did = identity.did().to_string();
    let agent = Gents::from_default_agent_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.agents().is_empty());
    assert_eq!(agent.node_did(), did);
    assert!(agent.default_agent_did().is_empty());
    assert!(agent.unavailable_agents().is_empty());
}

#[tokio::test]
async fn from_default_agent_documents_composes_behavior_and_inference_profile() {
    let (db, _owner_identity, owner) = test_node("composed-profile").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let did = identity.did().to_string();
    let default_agent_did = install_default_agent(&node, identity.as_ref(), &owner).await;

    let sampling_id = format!("{default_agent_did}:sampling");
    write_document(
        &owner,
        crate::Collection::InferenceSampling,
        &crate::documents::InferenceSampling {
            node_did: did.clone(),
            sampling_id: sampling_id.clone(),
            temperature: Some(0.2),
            top_p: Some(0.95),
            top_k: Some(40),
            min_p: Some(0.05),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(-0.25),
            repetition_penalty: Some(1.1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let profile_id = format!("{default_agent_did}:inference");
    let mut profile: crate::documents::InferenceProfile = read_document(
        &owner,
        &did,
        crate::Collection::InferenceProfile,
        &profile_id,
    )
    .await;
    profile.sampling_id = Some(sampling_id);
    profile.reasoning_effort = Some(crate::inference::resolved::ReasoningEffort::Max);
    profile.context_window = Some(32768);
    profile.max_output_tokens = Some(4096);
    write_document(&owner, crate::Collection::InferenceProfile, &profile)
        .await
        .unwrap();
    let execution_id = format!("{default_agent_did}:execution");
    write_document(
        &owner,
        crate::Collection::InferenceExecution,
        &crate::documents::InferenceExecution {
            node_did: did.clone(),
            execution_id: execution_id.clone(),
            max_turns: Some(8),
            stream_batch_ms: Some(500),
            stream_liveness_timeout_secs: Some(45),
            deadline_duration_secs: Some(120),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut profile: crate::documents::InferenceProfile = read_document(
        &owner,
        &did,
        crate::Collection::InferenceProfile,
        &profile_id,
    )
    .await;
    profile.execution_id = Some(execution_id);
    write_document(&owner, crate::Collection::InferenceProfile, &profile)
        .await
        .unwrap();
    let mut context = load_installed_context(&owner, &did, &default_agent_did).await;
    context.system_prompt = Some("You are precise.".to_string());
    upsert_context(&owner, &context).await;

    let backend: crate::documents::InferenceBackend = read_document(
        &owner,
        &did,
        crate::Collection::InferenceBackend,
        &format!("{default_agent_did}:backend"),
    )
    .await;
    assert_eq!(backend.endpoint, "http://127.0.0.1:1/v1");
    let agent = Gents::from_default_agent_documents(
        node,
        identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let behavior = agent
        .agents()
        .iter()
        .find(|behavior| behavior.agent_did == default_agent_did)
        .expect("default agent is runnable");
    assert_eq!(behavior.node_did(), did);
    assert_eq!(
        behavior.backend_id.as_deref(),
        Some(format!("{default_agent_did}:backend").as_str())
    );
    assert_eq!(behavior.model_name, "test-model");
    assert_eq!(behavior.context_window, 32768);
    assert_eq!(behavior.max_output_tokens, 4096);
    assert_eq!(behavior.max_turns, 8);
    assert_eq!(behavior.system_prompt, "You are precise.");
    assert!(matches!(
        behavior.compaction_strategy(),
        crate::inference::compaction::CompactionStrategy::StripThenSummarize
    ));
    assert_eq!(behavior.compaction_threshold(), 0.75);
    assert_eq!(behavior.stream_batch_ms, 500);
    assert_ne!(
        behavior.stream_liveness_timeout,
        Duration::from_secs(crate::inference::resolved::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),
        "the explicit execution setting must override the shorter default lease",
    );
    assert_eq!(behavior.stream_liveness_timeout, Duration::from_secs(45));
    assert_eq!(behavior.deadline_duration, Duration::from_secs(120));

    assert_eq!(behavior.sampling.temperature, Some(0.2));
    assert_eq!(behavior.sampling.top_p, Some(0.95));
    assert_eq!(behavior.sampling.top_k, Some(40));
    assert_eq!(behavior.sampling.min_p, Some(0.05));
    assert_eq!(behavior.sampling.frequency_penalty, Some(0.5));
    assert_eq!(behavior.sampling.presence_penalty, Some(-0.25));
    assert_eq!(behavior.sampling.repetition_penalty, Some(1.1));
    assert_eq!(
        behavior.sampling.reasoning_effort,
        Some(crate::inference::resolved::ReasoningEffort::Max)
    );

    let params = behavior
        .sampling
        .additional_params()
        .expect("pinned sampling knobs must produce provider body params");
    assert_eq!(params["top_p"], 0.95);
    assert_eq!(params["top_k"], 40);
    assert_eq!(params["repetition_penalty"], 1.1);
}

async fn read_document<T>(
    access: &ConfigAccess,
    owner: &str,
    collection: crate::Collection,
    id: &str,
) -> T
where
    T: serde::de::DeserializeOwned,
{
    let owner = owner.to_owned();
    let id = id.to_owned();
    let value = access
        .transact_readonly("test.read_configuration", |txn| {
            let owner = &owner;
            let id = &id;
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    crate::store::config_client::read_desired_state_record_in_txn(
                        txn, collection, owner, id,
                    )
                    .await?
                    .expect("installed document")
                    .1,
                )
            })
        })
        .await
        .unwrap();
    serde_json::from_value(value).unwrap()
}

async fn write_document<T>(
    owner: &ConfigAccess,
    collection: crate::Collection,
    document: &T,
) -> anyhow::Result<()>
where
    T: serde::Serialize,
{
    let value = serde_json::to_value(document)?;
    let plan = crate::store::config_client::DesiredStateApplyPlan::new(vec![
        crate::store::config_client::DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        },
    ])?;
    owner
        .transact_write("test.write_configuration", |txn| {
            let plan = &plan;
            Box::pin(async move { crate::documents::apply_desired_state_plan(txn, plan).await })
        })
        .await?;
    Ok(())
}

async fn load_installed_context(
    access: &ConfigAccess,
    did: &str,
    agent_did: &str,
) -> crate::documents::AgentContext {
    let behavior: Agent = read_document(access, did, crate::Collection::Agent, agent_did).await;
    read_document(
        access,
        did,
        crate::Collection::AgentContext,
        behavior.context_id.as_deref().expect("chain context"),
    )
    .await
}

async fn upsert_context(owner: &ConfigAccess, context: &crate::documents::AgentContext) {
    write_document(owner, crate::Collection::AgentContext, context)
        .await
        .unwrap();
}

#[tokio::test]
async fn from_default_agent_documents_resolves_tool_selection_with_ceiling() {
    let (db, _owner_identity, owner) = test_node("tool-selection").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let did = identity.did().to_string();
    let default_agent_did = install_default_agent(&node, identity.as_ref(), &owner).await;

    let researcher = install_agent(&node, identity.as_ref(), &owner, "researcher").await;
    write_document(
        &owner,
        crate::Collection::AgentTarget,
        &AgentTargetDocument {
            target_id: format!("{default_agent_did}:researcher"),
            node_did: did.clone(),
            target_node_did: did.clone(),
            agent_did: researcher,
            name: "researcher".to_string(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await
    .unwrap();
    let mut tools = load_installed_tools(&owner, &did, &default_agent_did).await;
    tools.host = Some(
        serde_json::from_value(
            serde_json::json!({"files":{"mode":"ReadWrite"},"bash":{"mode":"Unrestricted"}}),
        )
        .unwrap(),
    );
    tools.agents = Some(crate::documents::AgentTools {
        target_ids: vec![format!("{default_agent_did}:researcher")],
        enabled: Some(true),
    });
    upsert_tools(&owner, &tools).await;

    let agent = Gents::from_default_agent_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let behavior = agent
        .agents()
        .iter()
        .find(|behavior| behavior.agent_did == default_agent_did)
        .expect("default agent is runnable");
    assert_eq!(behavior.tools.host_tools(), &ToolSet::readonly());
    assert!(!behavior.tools.meta_tools_requested());
    assert_eq!(
        behavior
            .tools
            .agent_tools()
            .targets
            .iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>(),
        ["researcher".to_string()]
    );
    assert!(behavior.tools.agent_tools().enabled);
    let snapshot =
        resolve_document_runtime_snapshot(&agent.node, agent.document_runtime_context().unwrap())
            .await
            .unwrap();
    let tool_surface = snapshot
        .tool_surfaces
        .get(&default_agent_did)
        .expect("tool surface for default behavior");
    let tool_names = tool_surface.tool_names();
    assert!(tool_names.contains(&"agent_new".to_string()));
    assert!(tool_names.contains(&"agent_message".to_string()));
}

async fn load_installed_tools(access: &ConfigAccess, did: &str, agent_did: &str) -> Tools {
    let context = load_installed_context(access, did, agent_did).await;
    read_document(
        access,
        did,
        crate::Collection::Tools,
        context.tools_id.as_deref().expect("chain tools"),
    )
    .await
}

async fn upsert_tools(owner: &ConfigAccess, tools: &Tools) {
    write_document(owner, crate::Collection::Tools, tools)
        .await
        .unwrap();
}

async fn upsert_agent(owner: &ConfigAccess, agent: &Agent) -> anyhow::Result<()> {
    write_document(owner, crate::Collection::Agent, agent).await
}

#[tokio::test]
async fn from_default_agent_documents_filters_inactive_agent_targets() {
    let (db, _owner_identity, owner) = test_node("agent-target-disabled").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let did = identity.did().to_string();
    let default_agent_did = install_default_agent(&node, identity.as_ref(), &owner).await;

    let disabled_did = install_agent(&node, identity.as_ref(), &owner, "disabled-researcher").await;
    let behavior: Agent =
        read_document(&owner, &did, crate::Collection::Agent, &disabled_did).await;
    let mut disabled = behavior;
    disabled.enabled = false;
    upsert_agent(&owner, &disabled).await.unwrap();
    write_document(
        &owner,
        crate::Collection::AgentTarget,
        &AgentTargetDocument {
            target_id: format!("{default_agent_did}:disabled-researcher"),
            node_did: did.clone(),
            target_node_did: did.clone(),
            agent_did: disabled_did.clone(),
            name: "disabled-researcher".to_string(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await
    .unwrap();
    let mut tools = load_installed_tools(&owner, &did, &default_agent_did).await;
    tools.agents = Some(crate::documents::AgentTools {
        target_ids: vec![format!("{default_agent_did}:disabled-researcher")],
        enabled: Some(true),
    });
    upsert_tools(&owner, &tools).await;

    let snapshot = resolve_document_runtime_snapshot(
        &node,
        &DocumentResolveContext::for_tests(
            identity,
            ToolCeiling::readonly(),
            crate::inference::backends::health::BackendHealthMap::new(),
        ),
    )
    .await
    .unwrap();
    assert!(snapshot.agents.contains_key(&default_agent_did));
    let surface = snapshot
        .tool_surfaces
        .get(&default_agent_did)
        .expect("parent tool surface");
    assert!(surface.agent_targets().is_empty());
    let unavailable = snapshot
        .unavailable_agents
        .get(&disabled_did)
        .expect("disabled target agent must remain unavailable");
    assert_eq!(
        unavailable.public_reason,
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::AgentDisabled
    );
}

#[tokio::test]
async fn from_default_agent_documents_rejects_unresolved_agent_target() {
    let (db, _owner_identity, owner) = test_node("agent-target-missing").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let did = identity.did().to_string();
    let default_agent_did = install_default_agent(&node, identity.as_ref(), &owner).await;

    let target_did =
        install_agent(&node, identity.as_ref(), &owner, "target-before-corruption").await;
    write_document(
        &owner,
        crate::Collection::AgentTarget,
        &AgentTargetDocument {
            target_id: format!("{default_agent_did}:missing-behavior"),
            node_did: did.clone(),
            target_node_did: did.clone(),
            agent_did: target_did,
            name: "missing-behavior".to_string(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await
    .unwrap();
    let mut tools = load_installed_tools(&owner, &did, &default_agent_did).await;
    tools.agents = Some(crate::documents::AgentTools {
        target_ids: vec![format!("{default_agent_did}:missing-behavior")],
        enabled: Some(true),
    });
    upsert_tools(&owner, &tools).await;

    let target_id = format!("{default_agent_did}:missing-behavior");
    owner.transact_write("test.corrupt_target", |txn| {
        let did = &did;
        let target_id = &target_id;
        Box::pin(async move {
            let (doc_id, _) = crate::store::config_client::read_desired_state_record_in_txn(
                txn, crate::Collection::AgentTarget, did, target_id,
            ).await?.expect("installed target");
            let mutation = format!(r#"mutation($input: AgentTargetMutationInputArg!) {{ update_AgentTarget(docID: "{}", input: $input) {{ _docID }} }}"#,
                crate::store::graphql::escape_graphql_string(&doc_id));
            txn.execute_client_signed_update(&mutation, &serde_json::json!({"input":{"agent_did":"missing-behavior"}}), gents_protocol::schemas::AGENT_TARGET).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let agent = Gents::from_default_agent_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(!agent
        .agents()
        .iter()
        .any(|behavior| behavior.agent_did == default_agent_did));
    let unavailable = agent
        .unavailable_agents()
        .get(default_agent_did.as_str())
        .expect("parent with unresolved target must be unavailable");
    assert_eq!(
        unavailable.public_reason,
        AgentReadinessUnavailableReason::ToolConfigurationInvalid
    );
    assert!(unavailable.diagnostic.contains("missing-behavior"));
}

#[tokio::test]
async fn from_default_agent_documents_loads_runnable_agents_and_tracks_unavailable() {
    let (db, _owner_identity, owner) = test_node("behavior-catalog").await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let did = identity.did().to_string();
    let code = install_agent(&node, identity.as_ref(), &owner, "code").await;
    let broken = install_agent(&node, identity.as_ref(), &owner, "broken").await;
    let disabled_did = install_agent(&node, identity.as_ref(), &owner, "disabled").await;
    let unhealthy = install_agent(&node, identity.as_ref(), &owner, "unhealthy").await;
    let mut disabled: Agent =
        read_document(&owner, &did, crate::Collection::Agent, &disabled_did).await;
    disabled.enabled = false;
    write_document(&owner, crate::Collection::Agent, &disabled)
        .await
        .unwrap();
    set_probe_status(&owner, &did, &format!("{code}:backend"), "healthy").await;
    set_probe_status(&owner, &did, &format!("{unhealthy}:backend"), "unhealthy").await;
    let profile_id = format!("{broken}:inference");
    owner.transact_write("test.corrupt_profile", |txn| {
        let did = &did;
        let profile_id = &profile_id;
        Box::pin(async move {
            let (doc_id, _) = crate::store::config_client::read_desired_state_record_in_txn(
                txn, crate::Collection::InferenceProfile, did, profile_id,
            ).await?.expect("installed profile");
            let mutation = format!(r#"mutation($input: InferenceProfileMutationInputArg!) {{ update_InferenceProfile(docID: "{}", input: $input) {{ _docID }} }}"#,
                crate::store::graphql::escape_graphql_string(&doc_id));
            txn.execute_client_signed_update(&mutation, &serde_json::json!({"input":{"backend_id":"backend-missing"}}), gents_protocol::schemas::INFERENCE_PROFILE).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let agent = Gents::from_default_agent_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let runnable_names = agent
        .agents()
        .iter()
        .map(|behavior| behavior.agent_did.as_str())
        .collect::<rapidhash::RapidHashSet<_>>();
    assert_eq!(agent.node_did(), did);
    assert_eq!(agent.agents().len(), 1);
    assert!(runnable_names.contains(code.as_str()));
    let broken_reason = agent
        .unavailable_agents()
        .get(&broken)
        .cloned()
        .expect("missing broken agent rejection");
    assert_eq!(
        broken_reason.public_reason,
        AgentReadinessUnavailableReason::InferenceProfileInvalid
    );
    assert!(
        broken_reason.diagnostic.contains("backend-missing"),
        "missing backend diagnostic: {}",
        broken_reason.diagnostic
    );
    let disabled_reason = agent
        .unavailable_agents()
        .get(&disabled_did)
        .cloned()
        .expect("missing disabled agent rejection");
    assert_eq!(
        disabled_reason.diagnostic,
        format!("agent {disabled_did} is disabled")
    );
    let unhealthy_reason = agent
        .unavailable_agents()
        .get(&unhealthy)
        .cloned()
        .expect("missing unhealthy agent rejection");
    assert_eq!(
        unhealthy_reason.public_reason,
        AgentReadinessUnavailableReason::BackendTemporarilyUnavailable
    );
}

async fn set_probe_status(access: &ConfigAccess, did: &str, backend_id: &str, status: &str) {
    crate::inference::backends::registry::set_backend_probe_status_as(
        access, did, backend_id, status,
    )
    .await
    .unwrap();
}
