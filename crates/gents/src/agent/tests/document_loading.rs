use std::sync::Arc;
use std::time::Duration;

use gents_protocol::row::BehaviorReadinessUnavailableReason;

use super::super::*;
use super::support::*;
use crate::default_behavior_id_for_agent;
use crate::document_config::{AgentBehavior, SubagentTargetDocument, Tools};
use crate::ensure_runtime_schemas;
use crate::tool_surface::ToolCeiling;
use crate::toolset::ToolSet;

/// Install an explicit canonical behavior/context/tools/profile/backend chain
/// (shared `test_support::install_test_behavior`) and bind it as the
/// principal's explicit default. `ensure_agent_principal` returns the
/// `AgentPrincipal` only and invents no executable configuration, so a test
/// that needs a runnable default chooses one deliberately.
async fn install_default_behavior_chain(node: &EmbeddedNode, did: &str, behavior_id: &str) {
    crate::test_support::install_test_behavior(node, did, behavior_id).await;
    crate::document_config::upsert_agent_principal(node, did, None, Some(behavior_id), true)
        .await
        .unwrap();
}

#[tokio::test]
async fn document_constructor_rejects_node_without_signing_did_before_migrations() {
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    let identity = Arc::new(test_identity("document-constructor-unsigned-node"));

    let error = match Gents::from_default_behavior_documents(
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
async fn from_default_behavior_documents_preserves_identity_only_principal() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("bootstrap-profile"));
    let did = identity.did().to_string();
    // Identity bootstrap deliberately creates no executable default.
    crate::ensure_agent_principal(node.as_ref(), &did)
        .await
        .unwrap();
    let agent = Gents::from_default_behavior_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(agent.behaviors().is_empty());
    assert_eq!(agent.agent_did(), did);
    assert!(agent.default_behavior_id().is_empty());
    assert!(agent.unavailable_behaviors().is_empty());
}

#[tokio::test]
async fn from_default_behavior_documents_composes_behavior_and_inference_profile() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("composed-profile"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);

    install_default_behavior_chain(node.as_ref(), &did, &default_behavior_id).await;
    // The #649 sampling pins: an explicit sampling document is the only way a
    // profile can express sampling beyond temperature; the chain it must reach
    // the provider request body through is behavior.sampling.
    let sampling_id = format!("{default_behavior_id}:sampling");
    write_document(
        node.as_ref(),
        crate::Collection::InferenceSampling,
        &crate::document_config::InferenceSampling {
            agent_did: did.clone(),
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
    let profile_id = format!("{default_behavior_id}:inference");
    let mut profile = crate::load_inference_profile(node.as_ref(), &did, &profile_id)
        .await
        .unwrap()
        .expect("installed chain profile");
    profile.sampling_id = Some(sampling_id);
    profile.reasoning_effort = Some(crate::config::ReasoningEffort::Max);
    profile.context_window = Some(32768);
    profile.max_output_tokens = Some(4096);
    crate::document_config::upsert_inference_profile(node.as_ref(), &profile)
        .await
        .unwrap();
    let execution_id = format!("{default_behavior_id}:execution");
    write_document(
        node.as_ref(),
        crate::Collection::InferenceExecution,
        &crate::document_config::InferenceExecution {
            agent_did: did.clone(),
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
    let mut profile = crate::load_inference_profile(node.as_ref(), &did, &profile_id)
        .await
        .unwrap()
        .expect("installed chain profile");
    profile.execution_id = Some(execution_id);
    crate::document_config::upsert_inference_profile(node.as_ref(), &profile)
        .await
        .unwrap();
    // Context owns literal system instructions.
    let mut context = load_installed_context(node.as_ref(), &did, &default_behavior_id).await;
    context.system_prompt = Some("You are precise.".to_string());
    upsert_context(node.as_ref(), &context).await;

    let backend: crate::document_config::InferenceBackend = read_document(
        node.as_ref(),
        &did,
        crate::Collection::InferenceBackend,
        &format!("{default_behavior_id}:backend"),
    )
    .await;
    assert_eq!(backend.endpoint, "http://127.0.0.1:1/v1");
    let agent = Gents::from_default_behavior_documents(
        node,
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let behavior = &agent.behaviors()[0];
    assert_eq!(behavior.behavior_id, default_behavior_id);
    assert_eq!(behavior.agent_did(), did);
    assert_eq!(
        behavior.backend_id.as_deref(),
        Some(format!("{default_behavior_id}:backend").as_str())
    );
    assert_eq!(behavior.model_name, "test-model");
    assert_eq!(behavior.context_window, 32768);
    assert_eq!(behavior.max_output_tokens, 4096);
    assert_eq!(behavior.max_turns, 8);
    assert_eq!(behavior.system_prompt, "You are precise.");
    assert!(matches!(
        behavior.compaction_strategy(),
        crate::compaction::CompactionStrategy::StripThenSummarize
    ));
    assert_eq!(behavior.compaction_threshold(), 0.75);
    assert_eq!(behavior.stream_batch_ms, 500);
    assert_eq!(behavior.stream_liveness_timeout, Duration::from_secs(45));
    assert_eq!(behavior.deadline_duration, Duration::from_secs(120));

    // #649: the profile's sampling knobs must land on the behavior. This hop
    // hardcoded `top_p: None, top_k: None`, so a profile could not express any
    // sampling beyond temperature and every agent silently inherited whatever
    // the served checkpoint's generation_config.json baked in.
    assert_eq!(behavior.sampling.temperature, Some(0.2));
    assert_eq!(behavior.sampling.top_p, Some(0.95));
    assert_eq!(behavior.sampling.top_k, Some(40));
    assert_eq!(behavior.sampling.min_p, Some(0.05));
    assert_eq!(behavior.sampling.frequency_penalty, Some(0.5));
    assert_eq!(behavior.sampling.presence_penalty, Some(-0.25));
    assert_eq!(behavior.sampling.repetition_penalty, Some(1.1));
    assert_eq!(
        behavior.sampling.reasoning_effort,
        Some(crate::config::ReasoningEffort::Max)
    );

    // ...and from the behavior they must reach the provider request body.
    let params = behavior
        .sampling
        .additional_params()
        .expect("pinned sampling knobs must produce provider body params");
    assert_eq!(params["top_p"], 0.95);
    assert_eq!(params["top_k"], 40);
    assert_eq!(params["repetition_penalty"], 1.1);
}

async fn read_document<T: serde::de::DeserializeOwned>(
    node: &EmbeddedNode,
    owner: &str,
    collection: crate::Collection,
    id: &str,
) -> T {
    let owner = owner.to_owned();
    let id = id.to_owned();
    let value = crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "test.read_configuration",
        |txn| {
            let owner = &owner;
            let id = &id;
            Box::pin(async move {
                Ok(crate::config_client::read_desired_state_record_in_txn(
                    txn, collection, owner, id,
                )
                .await?
                .expect("installed document")
                .1)
            })
        },
    )
    .await
    .unwrap();
    serde_json::from_value(value).unwrap()
}

async fn write_document<T: serde::Serialize>(
    node: &EmbeddedNode,
    collection: crate::Collection,
    document: &T,
) -> anyhow::Result<()> {
    let value = serde_json::to_value(document)?;
    let plan = crate::config_client::DesiredStateApplyPlan::new(vec![
        crate::config_client::DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        },
    ])?;
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "test.write_configuration",
        |txn| {
            let plan = &plan;
            Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
        },
    )
    .await?;
    Ok(())
}

async fn load_installed_context(
    node: &EmbeddedNode,
    did: &str,
    behavior_id: &str,
) -> crate::document_config::AgentContext {
    let behavior: AgentBehavior =
        read_document(node, did, crate::Collection::AgentBehavior, behavior_id).await;
    read_document(
        node,
        did,
        crate::Collection::AgentContext,
        behavior.context_id.as_deref().expect("chain context"),
    )
    .await
}

async fn upsert_context(node: &EmbeddedNode, context: &crate::document_config::AgentContext) {
    write_document(node, crate::Collection::AgentContext, context)
        .await
        .unwrap();
}

#[tokio::test]
async fn from_default_behavior_documents_resolves_tool_selection_with_ceiling() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("tool-selection"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);

    install_default_behavior_chain(node.as_ref(), &did, &default_behavior_id).await;
    // A second enabled behavior the subagent target can point at, plus the
    // scoped SubagentTarget document and the Tools.subagents selection.
    crate::test_support::install_test_behavior(node.as_ref(), &did, "researcher").await;
    write_document(
        node.as_ref(),
        crate::Collection::SubagentTarget,
        &SubagentTargetDocument {
            target_id: format!("{default_behavior_id}:researcher"),
            agent_did: did.clone(),
            target_agent_did: did.clone(),
            behavior_id: "researcher".to_string(),
            name: "researcher".to_string(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await
    .unwrap();
    let mut tools = load_installed_tools(node.as_ref(), &did, &default_behavior_id).await;
    tools.host = Some(
        serde_json::from_value(
            serde_json::json!({"files":{"mode":"ReadWrite"},"bash":{"mode":"Unrestricted"}}),
        )
        .unwrap(),
    );
    tools.subagents = Some(crate::document_config::SubagentTools {
        target_ids: vec![format!("{default_behavior_id}:researcher")],
        spawn_enabled: Some(true),
        steering_enabled: Some(true),
        background_enabled: Some(true),
        ..Default::default()
    });
    upsert_tools(node.as_ref(), &tools).await;

    let agent = Gents::from_default_behavior_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let behavior = &agent.behaviors()[0];
    assert_eq!(behavior.behavior_id, default_behavior_id);
    // The readonly ceiling still bounds the canonical chain: file/bash tools
    // remain read-only regardless of what the document would grant.
    assert_eq!(behavior.tools.host_tools(), &ToolSet::readonly());
    assert!(!behavior.tools.meta_tools_requested());
    assert_eq!(
        behavior
            .tools
            .subagent_tools()
            .targets
            .iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>(),
        ["researcher".to_string()]
    );
    assert!(behavior.tools.subagent_tools().spawn_enabled);
    assert!(behavior.tools.subagent_tools().steering_enabled);
    assert!(behavior.tools.subagent_tools().background_enabled);
    let snapshot = resolve_document_runtime_snapshot(
        agent.node.as_ref(),
        agent.document_runtime_context().unwrap(),
    )
    .await
    .unwrap();
    let tool_surface = snapshot
        .tool_surfaces
        .get(&default_behavior_id)
        .expect("tool surface for default behavior");
    let tool_names = tool_surface.tool_names();
    assert!(tool_names.contains(&"spawn_subagent".to_string()));
    assert!(tool_names.contains(&"wait_subagent".to_string()));
    assert!(tool_names.contains(&"list_subagents".to_string()));
    assert!(tool_names.contains(&"read_subagent".to_string()));
    assert!(tool_names.contains(&"steer_subagent".to_string()));
    assert!(tool_names.contains(&"cancel_subagent".to_string()));
}

async fn load_installed_tools(node: &EmbeddedNode, did: &str, behavior_id: &str) -> Tools {
    let context = load_installed_context(node, did, behavior_id).await;
    read_document(
        node,
        did,
        crate::Collection::Tools,
        context.tools_id.as_deref().expect("chain tools"),
    )
    .await
}

async fn upsert_tools(node: &EmbeddedNode, tools: &Tools) {
    write_document(node, crate::Collection::Tools, tools)
        .await
        .unwrap();
}

#[tokio::test]
async fn from_default_behavior_documents_filters_inactive_subagent_targets() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("subagent-target-disabled"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);

    install_default_behavior_chain(node.as_ref(), &did, &default_behavior_id).await;
    // The destination behavior exists but is disabled. The parent remains
    // runnable while the inactive target is omitted from its presented surface.
    crate::test_support::install_test_behavior(node.as_ref(), &did, "disabled-researcher").await;
    let behavior: AgentBehavior = read_document(
        node.as_ref(),
        &did,
        crate::Collection::AgentBehavior,
        "disabled-researcher",
    )
    .await;
    let mut disabled = behavior;
    disabled.enabled = false;
    crate::upsert_agent_behavior(node.as_ref(), &disabled)
        .await
        .unwrap();
    write_document(
        node.as_ref(),
        crate::Collection::SubagentTarget,
        &SubagentTargetDocument {
            target_id: format!("{default_behavior_id}:disabled-researcher"),
            agent_did: did.clone(),
            target_agent_did: did.clone(),
            behavior_id: "disabled-researcher".to_string(),
            name: "disabled-researcher".to_string(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await
    .unwrap();
    let mut tools = load_installed_tools(node.as_ref(), &did, &default_behavior_id).await;
    tools.subagents = Some(crate::document_config::SubagentTools {
        target_ids: vec![format!("{default_behavior_id}:disabled-researcher")],
        spawn_enabled: Some(true),
        ..Default::default()
    });
    upsert_tools(node.as_ref(), &tools).await;

    let snapshot = resolve_document_runtime_snapshot(
        node.as_ref(),
        &DocumentResolveContext {
            identity,
            tool_ceiling: ToolCeiling::readonly(),
            backend_health: crate::backend_health::BackendHealthMap::new(),
        },
    )
    .await
    .unwrap();
    assert!(snapshot.behaviors.contains_key(&default_behavior_id));
    let surface = snapshot
        .tool_surfaces
        .get(&default_behavior_id)
        .expect("parent tool surface");
    assert!(surface.subagent_targets().is_empty());
    let unavailable = snapshot
        .unavailable_behaviors
        .get("disabled-researcher")
        .expect("disabled target behavior must remain unavailable");
    assert_eq!(
        unavailable.public_reason,
        gents_protocol::row::BehaviorReadinessUnavailableReason::BehaviorDisabled
    );
}

#[tokio::test]
async fn from_default_behavior_documents_rejects_unresolved_subagent_target() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("subagent-target-missing"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);

    install_default_behavior_chain(node.as_ref(), &did, &default_behavior_id).await;
    crate::test_support::install_test_behavior(node.as_ref(), &did, "target-before-corruption")
        .await;
    // The target names a behavior_id that was never installed: scope resolution
    // must fail closed with a diagnostic naming the subagent_targets entry.
    write_document(
        node.as_ref(),
        crate::Collection::SubagentTarget,
        &SubagentTargetDocument {
            target_id: format!("{default_behavior_id}:missing-behavior"),
            agent_did: did.clone(),
            target_agent_did: did.clone(),
            behavior_id: "target-before-corruption".to_string(),
            name: "missing-behavior".to_string(),
            description: None,
            tags: Vec::new(),
        },
    )
    .await
    .unwrap();
    let mut tools = load_installed_tools(node.as_ref(), &did, &default_behavior_id).await;
    tools.subagents = Some(crate::document_config::SubagentTools {
        target_ids: vec![format!("{default_behavior_id}:missing-behavior")],
        spawn_enabled: Some(true),
        ..Default::default()
    });
    upsert_tools(node.as_ref(), &tools).await;

    let owner = crate::graphql::escape_graphql_string(&did);
    let target_id =
        crate::graphql::escape_graphql_string(&format!("{default_behavior_id}:missing-behavior"));
    let response = node.execute(&format!(r#"mutation {{ update_SubagentTarget(filter: {{agent_did: {{_eq: "{owner}"}}, target_id: {{_eq: "{target_id}"}}}}, input: {{behavior_id: "missing-behavior"}}) {{_docID}} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let agent = Gents::from_default_behavior_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(!agent
        .behaviors()
        .iter()
        .any(|behavior| behavior.behavior_id == default_behavior_id));
    let unavailable = agent
        .unavailable_behaviors()
        .get(default_behavior_id.as_str())
        .expect("parent with unresolved target must be unavailable");
    assert_eq!(
        unavailable.public_reason,
        BehaviorReadinessUnavailableReason::ToolConfigurationInvalid
    );
    assert!(unavailable.diagnostic.contains("missing-behavior"));
}

#[tokio::test]
async fn from_default_behavior_documents_loads_runnable_behaviors_and_tracks_unavailable() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("behavior-catalog"));
    let did = identity.did().to_string();
    crate::ensure_agent_principal(node.as_ref(), &did)
        .await
        .unwrap();
    for name in ["code", "broken", "disabled", "unhealthy"] {
        crate::test_support::install_test_behavior(node.as_ref(), &did, name).await;
    }
    let mut disabled: AgentBehavior = read_document(
        node.as_ref(),
        &did,
        crate::Collection::AgentBehavior,
        "disabled",
    )
    .await;
    disabled.enabled = false;
    write_document(node.as_ref(), crate::Collection::AgentBehavior, &disabled)
        .await
        .unwrap();
    set_probe_status(node.as_ref(), &did, "code:backend", "healthy").await;
    set_probe_status(node.as_ref(), &did, "unhealthy:backend", "unhealthy").await;
    // Guarded publication rejects dangling references. Corrupt one profile only
    // after installing a valid bundle to exercise the runtime's failure path.
    let owner = crate::graphql::escape_graphql_string(&did);
    let response = node.execute(&format!(r#"mutation {{ update_InferenceProfile(filter: {{agent_did: {{_eq: "{owner}"}}, profile_id: {{_eq: "broken:inference"}}}}, input: {{backend_id: "backend-missing"}}) {{_docID}} }}"#)).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let agent = Gents::from_default_behavior_documents(
        node,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let runnable_names = agent
        .behaviors()
        .iter()
        .map(|behavior| behavior.behavior_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(agent.agent_did(), did);
    assert_eq!(agent.behaviors().len(), 1);
    assert!(runnable_names.contains("code"));
    let broken_reason = agent
        .unavailable_behaviors()
        .get("broken")
        .cloned()
        .expect("missing broken behavior rejection");
    assert_eq!(
        broken_reason.public_reason,
        BehaviorReadinessUnavailableReason::InferenceProfileInvalid
    );
    assert!(
        broken_reason.diagnostic.contains("backend-missing"),
        "missing backend diagnostic: {}",
        broken_reason.diagnostic
    );
    let disabled_reason = agent
        .unavailable_behaviors()
        .get("disabled")
        .cloned()
        .expect("missing disabled behavior rejection");
    assert_eq!(disabled_reason.diagnostic, "behavior disabled is disabled");
    let unhealthy_reason = agent
        .unavailable_behaviors()
        .get("unhealthy")
        .cloned()
        .expect("missing unhealthy behavior rejection");
    assert_eq!(
        unhealthy_reason.public_reason,
        BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable
    );
}

async fn set_probe_status(node: &EmbeddedNode, did: &str, backend_id: &str, status: &str) {
    crate::backend_registry::set_backend_probe_status(node, did, backend_id, status)
        .await
        .unwrap();
}
