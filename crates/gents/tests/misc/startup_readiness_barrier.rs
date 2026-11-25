use std::sync::Arc;

use anyhow::Result;
use events::EventName;
use gents::defra_node::Node as EmbeddedNode;
use gents::runtime::startup_readiness::StartupReadinessOptions;
use gents::test_support::observers::{RecordingBuildFailureObserver, RecordingProcessObserver};
use gents::{DocumentRuntimeOptions, Gents, ProcessLifecycleState};
use gents_protocol::row::{
    AgentReadinessState, AgentReadinessUnavailableReason, NodeReadinessSnapshot,
};
use gents_store::graphql::escape_graphql_string;
use gents_tools::surface::ToolCeiling;
use tokio::sync::watch;

use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::{managed_agent_fixture, trial_store};
use gents_inference::test_support::scripted_backend::ScriptedBackend;

async fn wait_for_node_readiness(
    node: &EmbeddedNode,
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    agent_did: &str,
    state: AgentReadinessState,
    reason: Option<AgentReadinessUnavailableReason>,
) -> NodeReadinessSnapshot {
    let escaped_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeReadiness(
                filter: {{ node_did: {{ _eq: "{escaped_did}" }} }},
                limit: 1
            ) {{
                snapshot_json
            }}
        }}"#
    );
    let mut updates = node.subscribe(&[EventName::Update]);
    loop {
        let response = access.execute(&query).await.expect("read node readiness");
        if let Some(snapshot) = response
            .get("data")
            .and_then(|data| data.get("NodeReadiness"))
            .and_then(serde_json::Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("snapshot_json"))
            .and_then(serde_json::Value::as_str)
            .and_then(|json| serde_json::from_str::<NodeReadinessSnapshot>(json).ok())
        {
            if snapshot.process_state.accepts_work()
                && snapshot.agents.iter().any(|entry| {
                    entry.agent_did == agent_did && entry.state == state && entry.reason == reason
                })
            {
                return snapshot;
            }
        }
        updates
            .recv()
            .await
            .expect("runtime status update subscription closed");
    }
}

#[tokio::test]
async fn persistent_build_failure_demotes_instead_of_wedging_ready() -> Result<()> {
    let db = trial_store("startup-readiness-559").await;
    let fixture = managed_agent_fixture(&db).await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let mock_endpoint = ScriptedBackend::start("default", Vec::new())?;
    bind_managed_agent_backend(
        &db,
        &fixture,
        "backend-559",
        mock_endpoint.endpoint(),
        "default",
    )
    .await;

    crate::support::fixtures::set_backend_auth_as(
        &fixture.access,
        db.node_identity.did(),
        "backend-559",
        serde_json::json!({"kind": "environment", "variable": "GENTS_TEST_559_UNSET_KEY"}),
    )
    .await;
    assert!(
        std::env::var_os("GENTS_TEST_559_UNSET_KEY").is_none(),
        "the poison env var must not be set for this test to be honest"
    );

    let observer = Arc::new(RecordingProcessObserver::default());
    let build_failure_observer = Arc::new(RecordingBuildFailureObserver::default());
    let agent = Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            startup_build_failure_observer: Some(build_failure_observer.clone()),
            retry_policy: gents::runtime::agent::retry::RetryPolicy {
                max_retries: 3,
                base_delay_ms: 1,
                max_delay_ms: 5,
            },
            startup_readiness: StartupReadinessOptions {
                build_failure_budget: 2,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await?;
    let default_agent_did = agent.default_agent_did().to_string();
    assert_eq!(default_agent_did, fixture.agent.did());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));

    tokio::time::timeout(
        gents_store::test_support::STALL_CEILING,
        observer.wait_for(ProcessLifecycleState::Ready),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "persistent startup failure did not reach Ready; states={:?}, failures={:?}",
            observer.states(),
            build_failure_observer.failures()
        )
    });

    let failures = build_failure_observer.failures();
    assert_eq!(failures.len(), 2, "the configured budget must be exhausted");
    assert_eq!(failures[0].failure_number, 1);
    assert_eq!(failures[1].failure_number, 2);
    assert!(failures
        .iter()
        .all(|failure| failure.agent_did == default_agent_did));
    assert!(failures.iter().all(|failure| failure.budget == 2));
    assert!(failures
        .iter()
        .all(|failure| failure.error.contains("GENTS_TEST_559_UNSET_KEY")));

    let readiness = wait_for_node_readiness(
        node.as_ref(),
        &fixture.access,
        identity.did(),
        &default_agent_did,
        AgentReadinessState::Unavailable,
        Some(AgentReadinessUnavailableReason::ExecutorStartFailed),
    )
    .await;
    assert_eq!(readiness.agents.len(), 1);

    let _ = shutdown_tx.send(true);
    run_task.await??;

    let observed = observer.states();
    assert!(
        observed.contains(&ProcessLifecycleState::Ready),
        "process must reach Ready despite the un-buildable agent; observed {observed:?}"
    );

    Ok(())
}

#[tokio::test]
async fn transient_build_failure_within_budget_still_reaches_ready_healthy() -> Result<()> {
    use std::ffi::OsString;
    use std::sync::LazyLock;

    static ENV_VAR_LOCK: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));
    let _env_guard = ENV_VAR_LOCK.lock().await;

    struct RestoreEnv(&'static str, Option<OsString>);
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            unsafe {
                match &self.1 {
                    Some(value) => std::env::set_var(self.0, value),
                    None => std::env::remove_var(self.0),
                }
            }
        }
    }
    const VAR: &str = "GENTS_TEST_559_LATE_KEY";
    let _restore = RestoreEnv(VAR, std::env::var_os(VAR));
    unsafe {
        std::env::remove_var(VAR);
    }

    let db = trial_store("startup-readiness-559-transient").await;
    let fixture = managed_agent_fixture(&db).await;
    let node = db.node.clone();
    let identity = db.node_identity.clone();
    let mock_endpoint = ScriptedBackend::start("default", Vec::new())?;
    bind_managed_agent_backend(
        &db,
        &fixture,
        "backend-559-late",
        mock_endpoint.endpoint(),
        "default",
    )
    .await;
    crate::support::fixtures::set_backend_auth_as(
        &fixture.access,
        db.node_identity.did(),
        "backend-559-late",
        serde_json::json!({"kind": "environment", "variable": VAR}),
    )
    .await;

    let observer = Arc::new(RecordingProcessObserver::default());
    let build_failure_observer = Arc::new(RecordingBuildFailureObserver::default());
    let agent = Gents::from_default_agent_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            retry_policy: gents::runtime::agent::retry::RetryPolicy {
                max_retries: 3,
                base_delay_ms: 50,
                max_delay_ms: 100,
            },
            startup_readiness: StartupReadinessOptions {
                build_failure_budget: 10,
                ..Default::default()
            },
            process_state_observer: Some(observer.clone()),
            startup_build_failure_observer: Some(build_failure_observer.clone()),
            ..Default::default()
        },
    )
    .await?;
    let default_agent_did = agent.default_agent_did().to_string();
    assert_eq!(default_agent_did, fixture.agent.did());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));

    build_failure_observer.wait_for_failure_count(1).await;
    unsafe {
        std::env::set_var(VAR, "late-key");
    }

    observer.wait_for(ProcessLifecycleState::Ready).await;

    let failures = build_failure_observer.failures();
    assert!(
        !failures.is_empty(),
        "the key must be installed only after an observed failed build"
    );
    assert!(failures.iter().all(|failure| failure.budget == 10));
    assert!(failures
        .iter()
        .all(|failure| failure.agent_did == default_agent_did));
    assert!(failures.iter().all(|failure| failure.error.contains(VAR)));

    let readiness = wait_for_node_readiness(
        node.as_ref(),
        &fixture.access,
        identity.did(),
        &default_agent_did,
        AgentReadinessState::Ready,
        None,
    )
    .await;
    assert_eq!(readiness.agents.len(), 1);

    let _ = shutdown_tx.send(true);
    run_task.await??;
    Ok(())
}
