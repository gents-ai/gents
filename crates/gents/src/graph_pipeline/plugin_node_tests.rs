//! A plugin node end to end: a graph run's entry document reaches the plugin
//! through the callback engine, and the plugin's result lands as the node's
//! output document, correlated to the run.

use std::sync::Arc;
use std::time::{Duration, Instant};

use defra_node::EmbeddedNode;
use serde_json::{json, Value};

use super::runtime::graph_test_owner;
use super::*;

fn port(name: &str, collection: &str) -> PortSpec {
    PortSpec {
        name: name.to_owned(),
        collection: collection.to_owned(),
        schema: format!("{collection}/v1"),
        correlation_field: "graph_run_id".to_owned(),
        cardinality: PortCardinality::One,
        required: true,
    }
}

fn echo_plan(digest: &str, max_attempts: Option<u32>) -> GraphPlan {
    let at = |port: &str| PortRef {
        node_id: "echo".to_owned(),
        port: port.to_owned(),
    };
    compile_graph(
        &GraphIntent {
            agent_did: graph_test_owner().to_owned(),
            tags: vec![],
            graph_id: "echo-pipeline".to_owned(),
            nodes: vec![GraphNode {
                node_id: "echo".to_owned(),
                capability_id: "echo".to_owned(),
                capability_revision: "v1".to_owned(),
            }],
            edges: vec![],
            entries: vec![EntryBinding {
                name: "input".to_owned(),
                collection: "EchoInput".to_owned(),
                schema: "EchoInput/v1".to_owned(),
                input_contract: None,
                to: at("input"),
            }],
            results: vec![ResultContract {
                name: "echoed".to_owned(),
                from: at("echoed"),
                cardinality: ResultCardinality::Exactly { count: 1 },
                terminal: true,
            }],
            limits: GraphLimits {
                max_nodes: 1,
                max_edges: 1,
                max_depth: 1,
                max_fan_out: 1,
                max_total_invocations: 2,
                max_runtime_secs: 60,
            },
        },
        &[StageCapability {
            agent_did: graph_test_owner().to_owned(),
            tags: vec![],
            workspace_authority: None,
            capability_id: "echo".to_owned(),
            revision: "v1".to_owned(),
            target: StageTarget::Plugin {
                plugin: "team/plugin".to_owned(),
                digest: Some(digest.to_owned()),
                max_attempts,
            },
            input_ports: vec![port("input", "EchoInput")],
            output_ports: vec![port("echoed", "EchoOutput")],
            allowed_callers: vec![graph_test_owner().to_owned()],
        }],
        graph_test_owner(),
        &CompilerPolicy::default(),
    )
    .unwrap()
}

async fn rows(node: &EmbeddedNode, query: &str, collection: &str) -> Vec<Value> {
    crate::graphql::rows::<Value>(&node.execute(query).await, collection).unwrap()
}

/// Publishes and starts the echo graph with its plugin node pinned to
/// `digest`, and runs the callback engine over the echo plugin installed in a
/// fresh home until the run settles.
async fn run_echo_graph(
    digest: Option<&str>,
) -> (Arc<EmbeddedNode>, GraphRunReceipt, GraphRunView) {
    let (home, record) = crate::plugin::tests::executor::installed_echo();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node.add_schema(
        "type EchoInput { graph_run_id: String @index(unique: true) payload: String }
         type EchoOutput { graph_run_id: String @index payload: String }",
    )
    .await
    .unwrap();

    let plan = echo_plan(digest.unwrap_or(&record.digest), None);
    let materialized = materialize_graph_revision(&node, None, graph_test_owner(), &plan)
        .await
        .unwrap();
    assert!(
        materialized.task_ids.is_empty(),
        "a plugin node creates no model request"
    );
    assert!(rows(&node, "{ Trigger { trigger_id } }", "Trigger")
        .await
        .is_empty());
    let bindings = rows(
        &node,
        "{ CallbackBinding { binding_id input_fields } }",
        "CallbackBinding",
    )
    .await;
    assert_eq!(bindings.len(), 1);
    assert_eq!(
        bindings[0]["input_fields"],
        json!(["graph_run_id", "payload"])
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let engine = tokio::spawn(crate::callback::run_callback_engine(
        node.clone(),
        graph_test_owner().to_owned(),
        None,
        Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        ))),
        cancel.clone(),
    ));
    activate_graph_revision(
        &node,
        None,
        graph_test_owner(),
        "echo-pipeline",
        &plan.digest,
        None,
    )
    .await
    .unwrap();
    let run = start_graph_run(
        &node,
        None,
        graph_test_owner(),
        "echo-pipeline",
        None,
        "input",
        json!({ "payload": "hello" }),
    )
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backoff = Duration::from_millis(10);
    let view = loop {
        let view = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        if view.status != "running" || Instant::now() >= deadline {
            break view;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_millis(250));
    };
    cancel.cancel();
    let _ = engine.await;
    (node, run, view)
}

#[tokio::test]
async fn a_graph_run_reaches_a_plugin_node_and_its_result_is_the_nodes_output() {
    let (node, run, view) = run_echo_graph(None).await;
    assert_eq!(view.status, "succeeded", "{view:#?}");
    assert_eq!(
        rows(
            &node,
            "{ EchoOutput { graph_run_id payload } }",
            "EchoOutput"
        )
        .await,
        vec![json!({"graph_run_id": run.correlation, "payload": "hello"})]
    );
    assert_eq!(view.stages[0].succeeded, 1);
    assert!(view.requests.is_empty(), "no model request ran");
    node.shutdown().await;
}

#[tokio::test]
async fn a_plugin_node_that_cannot_run_fails_the_run_with_its_reason() {
    let other = format!("sha256:{}", "0".repeat(64));
    let (node, _, view) = run_echo_graph(Some(&other)).await;
    assert_eq!(view.status, "failed", "{view:#?}");
    let evidence = view.failure_evidence.expect("a failed run says why");
    assert!(
        evidence["message"]
            .as_str()
            .is_some_and(|message| message.contains("not the pinned")),
        "{evidence}"
    );
    assert!(rows(&node, "{ EchoOutput { payload } }", "EchoOutput")
        .await
        .is_empty());
    node.shutdown().await;
}

#[tokio::test]
async fn a_failed_plugin_node_is_retried_and_the_run_then_succeeds() {
    let (home, record) = crate::plugin::tests::executor::installed_echo();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node.add_schema(
        "type EchoInput { graph_run_id: String @index(unique: true) payload: String }
         type EchoOutput { graph_run_id: String @index payload: String }",
    )
    .await
    .unwrap();
    // With its artifact gone from the store, the first attempt cannot run
    // and fails; putting it back lets the retry succeed.
    let artifact = home.path().join(format!(
        "plugins/store/{}.afb",
        record.digest.strip_prefix("sha256:").unwrap()
    ));
    let bytes = std::fs::read(&artifact).unwrap();
    std::fs::remove_file(&artifact).unwrap();
    let plan = echo_plan(&record.digest, Some(2));
    materialize_graph_revision(&node, None, graph_test_owner(), &plan)
        .await
        .unwrap();
    // The engine runs before the revision is activated, as it does in a
    // runtime, so it has discovered the plugin node's route by the time the
    // run's first document is written.
    let cancel = tokio_util::sync::CancellationToken::new();
    let engine = tokio::spawn(crate::callback::run_callback_engine(
        node.clone(),
        graph_test_owner().to_owned(),
        None,
        Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        ))),
        cancel.clone(),
    ));
    activate_graph_revision(
        &node,
        None,
        graph_test_owner(),
        "echo-pipeline",
        &plan.digest,
        None,
    )
    .await
    .unwrap();
    let run = start_graph_run(
        &node,
        None,
        graph_test_owner(),
        "echo-pipeline",
        None,
        "input",
        json!({ "payload": "hello" }),
    )
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(30);
    let failed = loop {
        let invocations = rows(
            &node,
            "{ CallbackInvocation { lifecycle_state attempts } }",
            "CallbackInvocation",
        )
        .await;
        if invocations
            .first()
            .is_some_and(|row| row["lifecycle_state"] == "failed")
            || Instant::now() >= deadline
        {
            break invocations;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(failed[0]["lifecycle_state"], "failed", "{failed:?}");
    let waiting = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
        .await
        .unwrap();
    assert_eq!(
        waiting.status, "running",
        "a failure that will be retried does not fail the run"
    );

    std::fs::write(&artifact, &bytes).unwrap();
    let view = loop {
        let view = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        if view.status != "running" || Instant::now() >= deadline {
            break view;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    cancel.cancel();
    let _ = engine.await;
    assert_eq!(view.status, "succeeded", "{view:#?}");
    let invocation = &rows(
        &node,
        "{ CallbackInvocation { lifecycle_state attempts } }",
        "CallbackInvocation",
    )
    .await[0];
    assert_eq!(invocation["attempts"], 2, "the second attempt succeeded");
    node.shutdown().await;
}

/// A run started before the engine noticed the revision's routes still runs:
/// its first document is live work, not history.
#[tokio::test]
async fn a_run_started_before_its_routes_are_noticed_still_runs() {
    let (home, record) = crate::plugin::tests::executor::installed_echo();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    node.add_schema(
        "type EchoInput { graph_run_id: String @index(unique: true) payload: String }
         type EchoOutput { graph_run_id: String @index payload: String }",
    )
    .await
    .unwrap();
    let plan = echo_plan(&record.digest, None);
    materialize_graph_revision(&node, None, graph_test_owner(), &plan)
        .await
        .unwrap();
    activate_graph_revision(
        &node,
        None,
        graph_test_owner(),
        "echo-pipeline",
        &plan.digest,
        None,
    )
    .await
    .unwrap();
    let run = start_graph_run(
        &node,
        None,
        graph_test_owner(),
        "echo-pipeline",
        None,
        "input",
        json!({ "payload": "hello" }),
    )
    .await
    .unwrap();

    // Only now does the engine start, and notice the route.
    let cancel = tokio_util::sync::CancellationToken::new();
    let engine = tokio::spawn(crate::callback::run_callback_engine(
        node.clone(),
        graph_test_owner().to_owned(),
        None,
        Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        ))),
        cancel.clone(),
    ));
    let deadline = Instant::now() + Duration::from_secs(30);
    let view = loop {
        let view = reconcile_graph_run(&node, None, graph_test_owner(), &run.run_id)
            .await
            .unwrap();
        if view.status != "running" || Instant::now() >= deadline {
            break view;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    cancel.cancel();
    let _ = engine.await;
    assert_eq!(view.status, "succeeded", "{view:#?}");
    node.shutdown().await;
}
