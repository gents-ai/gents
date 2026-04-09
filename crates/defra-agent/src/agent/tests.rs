use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rig::completion::ToolDefinition;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;

use super::supervision::supervise_behaviors_with_runner;
use super::*;
use crate::default_behavior_id_for_agent;
use crate::document_config::{AgentBehavior, ToolSelectionDocument};
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::identity::SimpleIdentity;
use crate::tool_surface::ToolCeiling;
use crate::toolset::ToolSet;

async fn test_node() -> Arc<EmbeddedNode> {
    Arc::new(EmbeddedNode::builder().build().await.unwrap())
}

fn test_identity(name: &str) -> SimpleIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    SimpleIdentity::new(name, path, None)
}

#[derive(Debug, Deserialize)]
struct EchoArgs {
    value: String,
}

#[derive(Debug, thiserror::Error)]
#[error("echo tool error")]
struct EchoToolError;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EchoTool;

impl Tool for EchoTool {
    const NAME: &'static str = "echo_value";

    type Error = EchoToolError;
    type Args = EchoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Echo a value back".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "value": {
                        "type": "string",
                        "description": "Value to echo"
                    }
                },
                "required": ["value"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(args.value)
    }
}

#[tokio::test]
async fn from_default_behavior_documents_marks_unbound_default_behavior_unavailable() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("bootstrap-profile"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);

    let agent = DefraAgent::from_default_behavior_documents(
        node,
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(agent.behaviors().is_empty());
    assert_eq!(agent.default_behavior_id(), default_behavior_id);
    assert_eq!(agent.agent_did(), did);
    assert_eq!(
        agent
            .unavailable_behaviors()
            .get(default_behavior_id.as_str())
            .map(String::as_str),
        Some(format!("behavior {default_behavior_id} has no backend binding").as_str())
    );
}

#[tokio::test]
async fn from_default_behavior_documents_composes_behavior_and_inference_profile() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("composed-profile"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);

    crate::ensure_agent_principal(node.as_ref(), &did)
        .await
        .unwrap();
    insert_backend(
        node.as_ref(),
        "backend-balanced",
        "http://127.0.0.1:8123/v1",
    )
    .await;
    insert_inference_profile(node.as_ref(), "balanced").await;
    update_default_behavior(
        node.as_ref(),
        &default_behavior_id,
        "balanced",
        "You are precise.",
        "backend-balanced",
        "gpt-local",
        "Summarize",
        0.6,
    )
    .await;

    let agent = DefraAgent::from_default_behavior_documents(
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
    assert_eq!(behavior.name, default_behavior_id);
    assert_eq!(behavior.did(), did);
    assert_eq!(behavior.backend_endpoint, "http://127.0.0.1:8123/v1");
    assert_eq!(behavior.model_name, "gpt-local");
    assert_eq!(behavior.context_window, 32768);
    assert_eq!(behavior.max_output_tokens, 4096);
    assert_eq!(behavior.max_turns, 8);
    assert_eq!(behavior.system_prompt, "You are precise.");
    assert_eq!(behavior.backend_id.as_deref(), Some("backend-balanced"));
    assert!(matches!(
        behavior.compaction_strategy,
        crate::compaction::CompactionStrategy::Summarize
    ));
    assert_eq!(behavior.compaction_threshold, 0.6);
    assert_eq!(behavior.stream_batch_ms, 500);
    assert_eq!(behavior.deadline_duration, Duration::from_secs(120));
}

#[tokio::test]
async fn from_default_behavior_documents_resolves_tool_selection_with_ceiling() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("tool-selection"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);
    let selection_id = format!("{default_behavior_id}:tools");

    let bootstrap = crate::ensure_agent_principal(node.as_ref(), &did)
        .await
        .unwrap();
    insert_backend(node.as_ref(), "backend-tools", "http://127.0.0.1:8222/v1").await;
    crate::upsert_tool_selection(
        node.as_ref(),
        &ToolSelectionDocument {
            selection_id: selection_id.clone(),
            agent_did: did.clone(),
            display_name: Some("Ops".to_string()),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadWrite".to_string()),
            enable_bash: Some(true),
            bash_mode: Some("Unrestricted".to_string()),
            cli_tool_names: Some(Vec::new()),
            enable_meta_tools: Some(false),
            delegate_to: Some(vec!["did:defra-agent:amy-code".to_string()]),
        },
    )
    .await
    .unwrap();
    crate::upsert_agent_behavior(
        node.as_ref(),
        &AgentBehavior {
            behavior_id: bootstrap.default_behavior.behavior_id,
            agent_did: did.clone(),
            display_name: Some("Default".to_string()),
            system_prompt: Some("Use tools carefully.".to_string()),
            backend_id: Some("backend-tools".to_string()),
            model_name: None,
            tool_selection_id: Some(selection_id),
            inference_profile_id: None,
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.75),
            enabled: true,
            created_at: bootstrap.default_behavior.created_at,
        },
    )
    .await
    .unwrap();

    let agent = DefraAgent::from_default_behavior_documents(
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
    assert_eq!(behavior.name, default_behavior_id);
    assert_eq!(behavior.tools.host_tools(), &ToolSet::readonly());
    assert!(!behavior.tools.meta_tools_requested());
    assert_eq!(
        behavior.tools.delegate_to(),
        ["did:defra-agent:amy-code".to_string()]
    );
}

#[tokio::test]
async fn from_default_behavior_documents_loads_runnable_behaviors_and_tracks_unavailable() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("behavior-catalog"));
    let did = identity.did().to_string();
    let default_behavior_id = default_behavior_id_for_agent(&did);

    crate::ensure_agent_principal(node.as_ref(), &did)
        .await
        .unwrap();
    insert_backend_with_health(
        node.as_ref(),
        "backend-healthy",
        "http://127.0.0.1:8444/v1",
        true,
        "healthy",
    )
    .await;
    insert_backend_with_health(
        node.as_ref(),
        "backend-unhealthy",
        "http://127.0.0.1:8555/v1",
        true,
        "unhealthy",
    )
    .await;
    crate::upsert_agent_behavior(
        node.as_ref(),
        &AgentBehavior {
            behavior_id: format!("{did}:code"),
            agent_did: did.clone(),
            display_name: Some("Code".to_string()),
            system_prompt: Some("You write code.".to_string()),
            backend_id: Some("backend-healthy".to_string()),
            model_name: Some("gpt-code".to_string()),
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.7),
            enabled: true,
            created_at: None,
        },
    )
    .await
    .unwrap();
    crate::upsert_agent_behavior(
        node.as_ref(),
        &AgentBehavior {
            behavior_id: format!("{did}:broken"),
            agent_did: did.clone(),
            display_name: Some("Broken".to_string()),
            system_prompt: Some("This backend is missing.".to_string()),
            backend_id: Some("backend-missing".to_string()),
            model_name: Some("gpt-missing".to_string()),
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.7),
            enabled: true,
            created_at: None,
        },
    )
    .await
    .unwrap();
    crate::upsert_agent_behavior(
        node.as_ref(),
        &AgentBehavior {
            behavior_id: format!("{did}:disabled"),
            agent_did: did.clone(),
            display_name: Some("Disabled".to_string()),
            system_prompt: Some("You should never run.".to_string()),
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.7),
            enabled: false,
            created_at: None,
        },
    )
    .await
    .unwrap();
    crate::upsert_agent_behavior(
        node.as_ref(),
        &AgentBehavior {
            behavior_id: format!("{did}:unhealthy"),
            agent_did: did.clone(),
            display_name: Some("Unhealthy".to_string()),
            system_prompt: Some("Backend is unhealthy.".to_string()),
            backend_id: Some("backend-unhealthy".to_string()),
            model_name: Some("gpt-unhealthy".to_string()),
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.7),
            enabled: true,
            created_at: None,
        },
    )
    .await
    .unwrap();

    let agent = DefraAgent::from_default_behavior_documents(
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
        .map(|behavior| behavior.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(agent.agent_did(), did);
    assert_eq!(agent.default_behavior_id(), default_behavior_id);
    assert_eq!(agent.behaviors().len(), 1);
    assert!(runnable_names.contains(format!("{did}:code").as_str()));
    let default_reason = agent
        .unavailable_behaviors()
        .get(default_behavior_id.as_str())
        .cloned()
        .expect("missing default behavior rejection");
    assert_eq!(
        default_reason,
        format!("behavior {default_behavior_id} has no backend binding")
    );
    let broken_reason = agent
        .unavailable_behaviors()
        .get(format!("{did}:broken").as_str())
        .cloned()
        .expect("missing broken behavior rejection");
    assert!(broken_reason.contains("references missing backend backend-missing"));
    let disabled_reason = agent
        .unavailable_behaviors()
        .get(format!("{did}:disabled").as_str())
        .cloned()
        .expect("missing disabled behavior rejection");
    assert_eq!(
        disabled_reason,
        format!("behavior {did}:disabled is disabled")
    );
    let unhealthy_reason = agent
        .unavailable_behaviors()
        .get(format!("{did}:unhealthy").as_str())
        .cloned()
        .expect("missing unhealthy behavior rejection");
    assert!(unhealthy_reason.contains("backend backend-unhealthy is unavailable"));
}

#[tokio::test]
async fn builder_includes_custom_tools_in_resolved_tool_surface() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    insert_backend(node.as_ref(), "builder-backend", "http://127.0.0.1:8777/v1").await;
    let identity = Arc::new(test_identity("builder-custom-tools"));

    let agent = DefraAgent::builder()
        .node(node.clone())
        .identity(identity.clone())
        .tool_ceiling(ToolCeiling::meta_only())
        .behavior("policy-ops")
        .backend_id("builder-backend")
        .system_prompt("You manage policies.")
        .custom_tool(EchoTool)
        .done()
        .build()
        .await
        .unwrap();

    assert_eq!(agent.agent_did(), identity.did());
    assert_eq!(agent.default_behavior_id(), "policy-ops");
    assert!(agent.document_runtime_context().is_none());
    assert_eq!(
        agent.behaviors()[0].tools.custom_tool_names(),
        vec!["echo_value".to_string()]
    );

    let tool_surface = agent.behaviors()[0]
        .tools
        .resolve(node.as_ref())
        .await
        .unwrap();
    assert!(tool_surface
        .tool_names()
        .contains(&"echo_value".to_string()));
}

#[tokio::test]
async fn builder_requires_resolvable_backend_documents() {
    let node = test_node().await;
    ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let identity = Arc::new(test_identity("builder-missing-backend"));

    let error = match DefraAgent::builder()
        .node(node)
        .identity(identity)
        .behavior("policy-ops")
        .backend_id("missing-backend")
        .done()
        .build()
        .await
    {
        Ok(_) => panic!("builder should reject missing backend docs"),
        Err(error) => error,
    };

    assert!(error
        .to_string()
        .contains("behavior 'policy-ops' references missing backend missing-backend"));
}

#[tokio::test]
async fn supervision_restarts_panicking_behavior_while_sibling_continues() {
    let panic_attempts = Arc::new(AtomicUsize::new(0));
    let sibling_ticks = Arc::new(AtomicUsize::new(0));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let behaviors = vec![
        Arc::new(
            PendingBehaviorConfig::new("panic-profile")
                .build_with_identity_for_test(test_identity("panic-profile")),
        ),
        Arc::new(
            PendingBehaviorConfig::new("steady-profile")
                .build_with_identity_for_test(test_identity("steady-profile")),
        ),
    ];

    let runner = {
        let panic_attempts = panic_attempts.clone();
        let sibling_ticks = sibling_ticks.clone();
        move |behavior: Arc<crate::config::BehaviorConfig>, mut shutdown: watch::Receiver<bool>| {
            let panic_attempts = panic_attempts.clone();
            let sibling_ticks = sibling_ticks.clone();
            async move {
                if behavior.name == "panic-profile" {
                    let attempt = panic_attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt < 2 {
                        panic!("boom");
                    }
                }

                loop {
                    sibling_ticks.fetch_add(1, Ordering::SeqCst);
                    tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                    }
                }
            }
        }
    };

    let task = tokio::spawn(supervise_behaviors_with_runner(
        behaviors,
        shutdown_rx,
        crate::retry::RetryPolicy {
            max_retries: 3,
            base_delay_ms: 10,
            max_delay_ms: 25,
        },
        runner,
    ));

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if panic_attempts.load(Ordering::SeqCst) >= 3
                && sibling_ticks.load(Ordering::SeqCst) > 3
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("behaviors should restart and continue");
    assert!(panic_attempts.load(Ordering::SeqCst) >= 3);
    assert!(sibling_ticks.load(Ordering::SeqCst) > 3);

    let _ = shutdown_tx.send(true);
    task.await.unwrap().unwrap();
}

async fn insert_inference_profile(node: &EmbeddedNode, profile_id: &str) {
    let escaped_profile_id = escape_graphql_string(profile_id);
    let mutation = format!(
        r#"mutation {{
            create_InferenceProfile(input: {{
                profile_id: "{escaped_profile_id}",
                display_name: "Balanced",
                context_window: 32768,
                max_output_tokens: 4096,
                max_turns: 8,
                temperature: 0.2,
                stream_batch_ms: 500,
                deadline_duration_secs: 120
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
}

async fn insert_backend(node: &EmbeddedNode, backend_id: &str, endpoint: &str) {
    insert_backend_with_health(node, backend_id, endpoint, true, "healthy").await;
}

async fn insert_backend_with_health(
    node: &EmbeddedNode,
    backend_id: &str,
    endpoint: &str,
    enabled: bool,
    probe_status: &str,
) {
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_endpoint = escape_graphql_string(endpoint);
    let escaped_probe_status = escape_graphql_string(probe_status);
    let mutation = format!(
        r#"mutation {{
            create_InferenceBackend(input: {{
                backend_id: "{escaped_backend_id}",
                name: "Balanced Backend",
                endpoint: "{escaped_endpoint}",
                max_concurrent: 2,
                enabled: {enabled},
                models: ["default"],
                last_probe: "2026-04-09T00:00:00Z",
                probe_status: "{escaped_probe_status}"
            }}) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
}

#[allow(clippy::too_many_arguments)]
async fn update_default_behavior(
    node: &EmbeddedNode,
    behavior_id: &str,
    inference_profile_id: &str,
    system_prompt: &str,
    backend_id: &str,
    model_name: &str,
    compaction_strategy: &str,
    compaction_threshold: f64,
) {
    let escaped_behavior_id = escape_graphql_string(behavior_id);
    let escaped_inference_profile_id = escape_graphql_string(inference_profile_id);
    let escaped_system_prompt = escape_graphql_string(system_prompt);
    let escaped_backend_id = escape_graphql_string(backend_id);
    let escaped_model_name = escape_graphql_string(model_name);
    let escaped_compaction_strategy = escape_graphql_string(compaction_strategy);
    let mutation = format!(
        r#"mutation {{
            update_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{escaped_behavior_id}" }} }},
                input: {{
                    inference_profile_id: "{escaped_inference_profile_id}",
                    system_prompt: "{escaped_system_prompt}",
                    backend_id: "{escaped_backend_id}",
                    model_name: "{escaped_model_name}",
                    compaction_strategy: "{escaped_compaction_strategy}",
                    compaction_threshold: {compaction_threshold},
                    enabled: true
                }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
}
