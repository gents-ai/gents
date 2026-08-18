use std::sync::{Arc, Mutex};

use anyhow::Result;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::startup_readiness::StartupReadinessOptions;
use gents::{
    ensure_runtime_schemas, AgentIdentity, DocumentRuntimeOptions, Gents, ProcessLifecycleObserver,
    ProcessLifecycleState, ToolCeiling,
};
use serde_json::Value;
use tokio::sync::watch;

use crate::support::fixtures::bind_default_behavior_backend;
use crate::support::fixtures::test_identity;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::waits::wait_for_runtime_process_state;

#[derive(Default)]
struct RecordingObserver {
    states: Mutex<Vec<ProcessLifecycleState>>,
}

impl ProcessLifecycleObserver for RecordingObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        self.states
            .lock()
            .expect("recording observer mutex poisoned")
            .push(state);
    }
}

#[tokio::test]
async fn persistent_build_failure_demotes_instead_of_wedging_ready() -> Result<()> {
    let identity = Arc::new(test_identity("startup-readiness-559"));
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;
    let mock_endpoint = MockModelEndpoint::start("default")?;
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-559",
        mock_endpoint.endpoint(),
    )
    .await;

    let escaped_backend_id = escape_graphql_string("backend-559");
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                input: {{ api_key_env_var: "GENTS_TEST_559_UNSET_KEY" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert!(
        std::env::var_os("GENTS_TEST_559_UNSET_KEY").is_none(),
        "the poison env var must not be set for this test to be honest"
    );

    let observer = Arc::new(RecordingObserver::default());
    let agent = Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            retry_policy: gents::retry::RetryPolicy {
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
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    let escaped_did = escape_graphql_string(identity.did());
    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }}, limit: 1) {{
                runnable_behavior_count
                unavailable_behavior_count
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRuntime"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("AgentRuntime row");
    assert_eq!(
        row.get("runnable_behavior_count").and_then(Value::as_i64),
        Some(0),
        "the demoted behavior must not be counted runnable: {row}"
    );
    assert_eq!(
        row.get("unavailable_behavior_count")
            .and_then(Value::as_i64),
        Some(1),
        "the demotion must be visible as unavailable: {row}"
    );

    let _ = shutdown_tx.send(true);
    run_task.await??;

    let observed = observer
        .states
        .lock()
        .expect("recording observer mutex poisoned")
        .clone();
    assert!(
        observed.contains(&ProcessLifecycleState::Ready),
        "process must reach Ready despite the un-buildable behavior; observed {observed:?}"
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

    let identity = Arc::new(test_identity("startup-readiness-559-transient"));
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;
    let mock_endpoint = MockModelEndpoint::start("default")?;
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-559-late",
        mock_endpoint.endpoint(),
    )
    .await;
    let escaped_backend_id = escape_graphql_string("backend-559-late");
    let mutation = format!(
        r#"mutation {{
            update_InferenceBackend(
                filter: {{ backend_id: {{ _eq: "{escaped_backend_id}" }} }},
                input: {{ api_key_env_var: "{VAR}" }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);

    let agent = Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            retry_policy: gents::retry::RetryPolicy {
                max_retries: 3,
                base_delay_ms: 50,
                max_delay_ms: 100,
            },
            startup_readiness: StartupReadinessOptions {
                build_failure_budget: 10,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));

    tokio::time::sleep(std::time::Duration::from_millis(75)).await;
    unsafe {
        std::env::set_var(VAR, "late-key");
    }

    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    let escaped_did = escape_graphql_string(identity.did());
    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }}, limit: 1) {{
                runnable_behavior_count
                unavailable_behavior_count
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRuntime"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("AgentRuntime row");
    assert_eq!(
        row.get("runnable_behavior_count").and_then(Value::as_i64),
        Some(1),
        "a behavior that recovered within the budget must be healthy: {row}"
    );
    assert_eq!(
        row.get("unavailable_behavior_count")
            .and_then(Value::as_i64),
        Some(0),
        "no demotion may survive a successful start: {row}"
    );

    let _ = shutdown_tx.send(true);
    run_task.await??;
    Ok(())
}
