use std::sync::{Arc, Mutex};

use anyhow::Result;
use events::EventName;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::startup_readiness::StartupReadinessOptions;
use gents::{
    ensure_runtime_schemas, AgentIdentity, DocumentRuntimeOptions, Gents, ProcessLifecycleState,
    ToolCeiling,
};
use serde_json::Value;
use tokio::sync::watch;

use crate::support::fixtures::bind_default_behavior_backend;
use crate::support::fixtures::test_identity;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::waits::RecordingProcessObserver;

#[derive(Clone, Debug)]
struct BuildFailure {
    behavior_id: String,
    failure_number: u32,
    budget: u32,
    error: String,
}

struct RecordingBuildFailureObserver {
    failures: Mutex<Vec<BuildFailure>>,
    failure_count_tx: watch::Sender<usize>,
}

impl Default for RecordingBuildFailureObserver {
    fn default() -> Self {
        let (failure_count_tx, _) = watch::channel(0);
        Self {
            failures: Mutex::new(Vec::new()),
            failure_count_tx,
        }
    }
}

impl RecordingBuildFailureObserver {
    fn failures(&self) -> Vec<BuildFailure> {
        self.failures
            .lock()
            .expect("build failure observer mutex poisoned")
            .clone()
    }

    async fn wait_for_failure_count(&self, expected: usize) {
        let mut failure_count_rx = self.failure_count_tx.subscribe();
        loop {
            if self
                .failures
                .lock()
                .expect("build failure observer mutex poisoned")
                .len()
                >= expected
            {
                return;
            }
            failure_count_rx
                .changed()
                .await
                .expect("build failure observer closed");
        }
    }
}

impl gents::startup_readiness::StartupBuildFailureObserver for RecordingBuildFailureObserver {
    fn on_build_failure(&self, behavior_id: &str, failure_number: u32, budget: u32, error: &str) {
        let count = {
            let mut failures = self
                .failures
                .lock()
                .expect("build failure observer mutex poisoned");
            failures.push(BuildFailure {
                behavior_id: behavior_id.to_string(),
                failure_number,
                budget,
                error: error.to_string(),
            });
            failures.len()
        };
        self.failure_count_tx.send_replace(count);
    }
}

async fn wait_for_runtime_counts(
    node: &EmbeddedNode,
    agent_did: &str,
    runnable: i64,
    unavailable: i64,
) -> Value {
    let escaped_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentRuntime(filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }}, limit: 1) {{
                runnable_behavior_count
                unavailable_behavior_count
            }}
        }}"#
    );
    let mut updates = node.subscribe(&[EventName::Update]);
    loop {
        let response = node.execute(&query).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        if let Some(row) = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRuntime"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .cloned()
        {
            let observed_runnable = row.get("runnable_behavior_count").and_then(Value::as_i64);
            let observed_unavailable = row
                .get("unavailable_behavior_count")
                .and_then(Value::as_i64);
            if observed_runnable == Some(runnable) && observed_unavailable == Some(unavailable) {
                return row;
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

    let observer = Arc::new(RecordingProcessObserver::default());
    let build_failure_observer = Arc::new(RecordingBuildFailureObserver::default());
    let agent = Gents::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            process_state_observer: Some(observer.clone()),
            startup_build_failure_observer: Some(build_failure_observer.clone()),
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
    let default_behavior_id = agent.default_behavior_id().to_string();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let run_task = tokio::spawn(agent.run(shutdown_rx));

    observer.wait_for(ProcessLifecycleState::Ready).await;

    let failures = build_failure_observer.failures();
    assert_eq!(failures.len(), 2, "the configured budget must be exhausted");
    assert_eq!(failures[0].failure_number, 1);
    assert_eq!(failures[1].failure_number, 2);
    assert!(failures
        .iter()
        .all(|failure| failure.behavior_id == default_behavior_id));
    assert!(failures.iter().all(|failure| failure.budget == 2));
    assert!(failures
        .iter()
        .all(|failure| failure.error.contains("GENTS_TEST_559_UNSET_KEY")));

    let row = wait_for_runtime_counts(node.as_ref(), identity.did(), 0, 1).await;
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

    let observed = observer.states();
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

    let observer = Arc::new(RecordingProcessObserver::default());
    let build_failure_observer = Arc::new(RecordingBuildFailureObserver::default());
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
            process_state_observer: Some(observer.clone()),
            startup_build_failure_observer: Some(build_failure_observer.clone()),
            ..Default::default()
        },
    )
    .await?;
    let default_behavior_id = agent.default_behavior_id().to_string();
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
        .all(|failure| failure.behavior_id == default_behavior_id));
    assert!(failures.iter().all(|failure| failure.error.contains(VAR)));

    let row = wait_for_runtime_counts(node.as_ref(), identity.did(), 1, 0).await;
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
