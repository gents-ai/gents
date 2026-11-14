use std::time::Duration;

use anyhow::{ensure, Context, Result};
use gents::DocumentRuntimeOptions;
use gents_documents::BackendAuth;
use gents_inference::backends::setup::{connection_spec, InferenceAuthMethod, InferenceProviderId};
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;

use crate::support::live_inference::{bind_target, live_target, LiveUser};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_live_model_completes_through_configured_openrouter_adapter() -> Result<()> {
    let target = live_target().await;
    let db = test_db("live-openrouter-local").await;
    let actors = bind_target(&db, &target).await;
    let live = LiveUser::install(&db, &target).await;
    let connection = connection_spec(
        InferenceProviderId::OpenRouter,
        InferenceAuthMethod::ApiKey,
        target.endpoint(),
    )?;
    let mut backend = target.backend(actors.node_did());
    backend.name = connection.backend_name.into();
    backend.provider_kind = connection.provider_kind;
    backend.openai_wire_api = connection.openai_wire_api;
    backend.endpoint = connection.endpoint;
    backend.auth = BackendAuth::ApiKey {
        key: "local-openrouter-adapter-fixture".into(),
    };
    live.apply(
        "test.live_openrouter.configure",
        vec![(Collection::InferenceBackend, serde_json::to_value(backend)?)],
    )
    .await;
    gents_inference::backends::registry::set_backend_probe_status_as(
        &live.access,
        actors.node_did(),
        target.backend_id(),
        "healthy",
    )
    .await?;
    let configured = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents_tools::surface::ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;
    let agent = configured
        .agents()
        .iter()
        .find(|agent| agent.agent.agent_did == actors.agent_did())
        .context("configured OpenRouter Agent")?;
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        gents::run_openai_oneshot_admitted(
            db.node.clone(),
            agent,
            "Acknowledge this request in one short sentence. Do not call tools.",
            &db.home.backend_admission(),
        ),
    )
    .await
    .context("local OpenRouter adapter request deadline")??;
    ensure!(!result.response_text.trim().is_empty());
    let requests = live.access.execute(&format!(
        r#"{{AgentRequest(filter: {{session_id: {{_eq: "{}"}}}}) {{lifecycle_state terminal_output}}}}"#,
        escape_graphql_string(&result.session_id),
    )).await?;
    ensure!(
        requests["data"]["AgentRequest"]
            .as_array()
            .context("admitted request rows")?
            .iter()
            .any(|request| request["lifecycle_state"] == "completed"
                && request["terminal_output"]["kind"] == "message"),
        "the local OpenRouter adapter request did not persist its terminal message"
    );
    Ok(())
}
