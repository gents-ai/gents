mod support;

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

use support::fixtures::bind_default_behavior_backend;
use support::fixtures::test_identity;
use support::mock_endpoint::MockModelEndpoint;
use support::waits::wait_for_runtime_process_state;

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

/// The #559 regression, end to end on the production startup path.
///
/// The behavior is snapshot-RUNNABLE (backend healthy, references resolve) but
/// its build fails on every attempt: the backend's `api_key_env_var` names an
/// environment variable that is not set, which snapshot classification never
/// resolves and `completion_client_api_key()` bails on. On main this wedges the
/// readiness barrier forever — the slot hot-restarts, `wait_ready()` never
/// returns, the process never reports Ready, and the trigger engine never
/// starts. With the bounded budget the behavior is demoted instead: the process
/// reaches Ready without it, and the degradation is visible in the AgentRuntime
/// counts (`/healthz` reads them as degraded).
#[tokio::test]
async fn persistent_build_failure_demotes_instead_of_wedging_ready() -> Result<()> {
    let node = Arc::new(EmbeddedNode::builder().build().await?);
    ensure_runtime_schemas(node.as_ref()).await?;
    let identity = Arc::new(test_identity("startup-readiness-559"));
    let mock_endpoint = MockModelEndpoint::start("default")?;
    bind_default_behavior_backend(
        node.as_ref(),
        identity.did(),
        "backend-559",
        mock_endpoint.endpoint(),
    )
    .await;

    // The build poison: an env var nothing sets. Unique to this test so no
    // other test's environment can accidentally satisfy it.
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

    // On main this wait never completes: the barrier has no release path for a
    // behavior that cannot build. The helper's internal deadline turns the
    // #559 hang into a loud failure.
    wait_for_runtime_process_state(node.as_ref(), identity.did(), "ready").await;

    // The demotion is observable, not silent: the runnable count dropped and
    // the unavailable count carries the demoted behavior — the exact fields
    // /healthz derives `degraded` from.
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

    // Ready was genuinely reached through the normal lifecycle.
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

/// A build failure within the budget must not demote: the behavior that
/// recovers on a later attempt reaches Ready as a healthy behavior. Here the
/// env var appears after the first failure, so attempt two succeeds.
#[tokio::test]
async fn transient_build_failure_within_budget_still_reaches_ready_healthy() -> Result<()> {
    use std::ffi::OsString;
    use std::sync::LazyLock;

    // Serialize env mutation with any other test touching process env.
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

    let node = Arc::new(EmbeddedNode::builder().build().await?);
    ensure_runtime_schemas(node.as_ref()).await?;
    let identity = Arc::new(test_identity("startup-readiness-559-transient"));
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
                // Generous budget: the point is that recovery within it wins.
                build_failure_budget: 10,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));

    // Let at least one build attempt fail, then supply the key.
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
