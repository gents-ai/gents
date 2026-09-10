use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, load_inference_backend_in_txn, read_desired_state_record_in_txn,
    ConfigAccess, DesiredStateApplyPlan,
};
use gents::defra_node::{EmbeddedNode, HttpConfig};
use gents::document_config::{AgentPrincipal, BackendAuth, PackConfig};
use gents::{
    default_behavior_id_for_agent, ensure_runtime_schemas, AgentIdentity, Collection,
    DocumentRuntimeOptions, Gents, InferenceBackend, KeyIdentity, McpPool, ToolCeiling,
    DEFAULT_MAX_TURNS,
};
use tokio::sync::watch;

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_or_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_or_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let data_dir = PathBuf::from(env_or("GENTS_DATA_DIR", "./var/defradb"));
    let http_port = env_or_u16("GENTS_HTTP_PORT", 9191);
    let agent_name = env_or("GENTS_NAME", "demo");
    let backend_id = env_or("GENTS_BACKEND_ID", "demo-backend");
    let model_endpoint = env_or("GENTS_MODEL_ENDPOINT", "http://127.0.0.1:8000/v1");
    let model_name = env_or("GENTS_MODEL_NAME", "default");
    let system_prompt = std::env::var("GENTS_SYSTEM_PROMPT").unwrap_or_default();
    let deadline_secs = env_or_u64("GENTS_DEADLINE_SECS", 900);
    let key_path = std::env::var("GENTS_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| data_dir.join("keys").join(format!("{agent_name}.key")));

    let http_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), http_port);
    let identity = Arc::new(
        KeyIdentity::load_or_create(key_path, None)
            .context("creating or loading agent identity key")?,
    );
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(&data_dir)
            .with_http(HttpConfig::with_addr(http_addr))
            .with_node_identity_did(identity.did())
            .build()
            .await
            .context("building embedded DefraDB node")?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;
    seed_demo_documents(
        node.as_ref(),
        identity.did(),
        &backend_id,
        &model_endpoint,
        &model_name,
        &system_prompt,
        deadline_secs,
    )
    .await?;

    let agent = Gents::from_default_behavior_documents(
        node,
        identity.clone(),
        DocumentRuntimeOptions {
            mcp_pool: McpPool::new(),
            local_hostname: Some("localhost".to_string()),
            tool_ceiling: ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });

    tracing::info!(
        agent_name,
        agent_did = agent.agent_did(),
        graphql = %format!("http://127.0.0.1:{http_port}/api/v0/graphql"),
        backend_id,
        "serving default behavior"
    );

    agent.run(shutdown_rx).await
}

async fn seed_demo_documents(
    node: &EmbeddedNode,
    agent_did: &str,
    backend_id: &str,
    model_endpoint: &str,
    model_name: &str,
    system_prompt: &str,
    deadline_secs: u64,
) -> Result<()> {
    let deadline_secs = i64::try_from(deadline_secs).context("deadline exceeds supported range")?;
    ConfigAccess::transact_local(node, None, "example.default_config", |txn| {
        Box::pin(async move {
            let mut principal: AgentPrincipal = match read_desired_state_record_in_txn(
                txn, Collection::AgentPrincipal, agent_did, agent_did,
            ).await? {
                Some((_, value)) => serde_json::from_value(value)?,
                None => serde_json::from_value(serde_json::json!({"agent_did":agent_did}))?,
            };
            let behavior_id = principal.default_behavior_id.clone()
                .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));
            principal.default_behavior_id = Some(behavior_id.clone());
            let profile_id = format!("{behavior_id}:demo-profile");
            let tools_id = format!("{behavior_id}:demo-tools");
            let context_id = format!("{behavior_id}:demo-context");
            let execution_id = format!("{behavior_id}:demo-execution");
            let compaction_id = format!("{behavior_id}:demo-compaction");
            let created_at = read_desired_state_record_in_txn(
                txn, Collection::AgentBehavior, agent_did, &behavior_id,
            ).await?.and_then(|(_, value)| value.get("created_at").cloned());
            let mut backend = match load_inference_backend_in_txn(txn, agent_did, backend_id).await? {
                Some(existing) => existing,
                None => serde_json::from_value::<InferenceBackend>(serde_json::json!({
                    "agent_did":agent_did,"backend_id":backend_id,"name":backend_id,
                    "provider_kind":"OpenAiCompatible","endpoint":model_endpoint,
                    "auth":BackendAuth::Unauthenticated,"max_concurrent":2,"max_queue_depth":100
                }))?,
            };
            // Endpoint changes do not replace authentication, provider, or capacity controls.
            backend.name = backend_id.to_owned();
            backend.endpoint = model_endpoint.to_owned();
            backend.enabled = true;
            let config: PackConfig = serde_json::from_value(serde_json::json!({
                "agent_principal":principal,
                "agent_behaviors":[{"agent_did":agent_did,"behavior_id":behavior_id,
                    "display_name":"Default","context_id":context_id,"inference_profile_id":profile_id,"created_at":created_at}],
                "contexts":[{"agent_did":agent_did,"context_id":context_id,
                    "system_prompt":system_prompt,"tools_id":tools_id,"compaction_id":compaction_id}],
                "compactions":[{"agent_did":agent_did,"compaction_id":compaction_id,
                    "strategy":"StripThenSummarize","threshold":0.75}],
                "tools":[{"agent_did":agent_did,"tools_id":tools_id,"display_name":"Demo Tools",
                    "host":{"files":{"mode":"ReadOnly"},"bash":{"mode":"ReadOnly"}},
                    "built_ins":{"enable_goal_tools":true,"enable_goal_creation":false}}],
                "inference_backends":[backend],
                "inference_profiles":[{"agent_did":agent_did,"profile_id":profile_id,"display_name":"Demo",
                    "backend_id":backend_id,"model_name":model_name,"context_window":131072,
                    "max_output_tokens":32768,"execution_id":execution_id}],
                "inference_execution":[{"agent_did":agent_did,"execution_id":execution_id,
                    "max_turns":DEFAULT_MAX_TURNS,"stream_batch_ms":1000,
                    "stream_liveness_timeout_secs":60.min(deadline_secs.saturating_sub(1)),"deadline_duration_secs":deadline_secs}]
            }))?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            apply_desired_state_plan(txn, &plan).await?;
            Ok(())
        })
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::config_client::write_inference_backend_document;
    use serde_json::json;

    #[tokio::test]
    async fn example_uses_scoped_config_preserving_auth_and_capacity() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());
        for owner in ["did:test:demo-a", "did:test:demo-b"] {
            seed_demo_documents(
                &node,
                owner,
                "backend",
                "http://localhost:8000/v1",
                "model",
                "literal {{ task }}",
                900,
            )
            .await?;
        }
        let backend: InferenceBackend = serde_json::from_value(json!({
            "agent_did":"did:test:demo-a","backend_id":"backend","name":"Backend",
            "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"api_key","key":"preserved-key"},"max_concurrent":7,"max_queue_depth":17
        }))?;
        write_inference_backend_document(&access, &backend).await?;
        seed_demo_documents(
            &node,
            "did:test:demo-a",
            "backend",
            "http://localhost:9000/v1",
            "second-model",
            "literal {{ task }}",
            123,
        )
        .await?;
        access
            .transact("example.verify", |txn| {
                Box::pin(async move {
                    let backend = load_inference_backend_in_txn(txn, "did:test:demo-a", "backend")
                        .await?
                        .unwrap();
                    assert!(
                        matches!(backend.auth, BackendAuth::ApiKey {key} if key=="preserved-key")
                    );
                    assert_eq!(
                        (backend.max_concurrent, backend.max_queue_depth),
                        (Some(7), Some(17))
                    );
                    assert_eq!(backend.endpoint, "http://localhost:9000/v1");
                    let other = load_inference_backend_in_txn(txn, "did:test:demo-b", "backend")
                        .await?
                        .unwrap();
                    assert_eq!(other.endpoint, "http://localhost:8000/v1");
                    let id = default_behavior_id_for_agent("did:test:demo-a");
                    let (_, tools) = read_desired_state_record_in_txn(
                        txn,
                        Collection::Tools,
                        "did:test:demo-a",
                        &format!("{id}:demo-tools"),
                    )
                    .await?
                    .unwrap();
                    assert_eq!(tools["host"]["files"]["mode"], "ReadOnly");
                    assert_eq!(tools["host"]["bash"]["mode"], "ReadOnly");
                    let (_, context) = read_desired_state_record_in_txn(
                        txn,
                        Collection::AgentContext,
                        "did:test:demo-a",
                        &format!("{id}:demo-context"),
                    )
                    .await?
                    .unwrap();
                    assert_eq!(context["system_prompt"], "literal {{ task }}");
                    Ok(())
                })
            })
            .await?;
        assert!(seed_demo_documents(
            &node,
            "did:test:demo-a",
            "backend",
            "http://localhost:9000/v1",
            "model",
            "",
            u64::MAX
        )
        .await
        .is_err());
        Ok(())
    }
}
