use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::{extract::State, http::StatusCode, routing::get, Router};
use futures::FutureExt;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{CallbackWorkers, PLUGIN_CALLBACK_CONCURRENCY};
use crate::config_client::{
    apply_desired_state_plan, ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::plugin::executor::PluginExecutor;
use crate::plugin::store;
use crate::plugin::tests::executor::{asking_plugin_wat, installed_plugin};
use crate::triggers::callback::documents::{load_callback, load_invocation, CallbackInvocationDoc};
use crate::UpdateSubscriptionSource;

const OWNER: &str = crate::request::lifecycle::test_support::PIN_FIXED_DID;
const CALLBACK: &str = "saturation";
const WAIT: Duration = Duration::from_secs(10);
const SCENARIO_LIMIT: Duration = Duration::from_secs(20);

#[derive(Clone)]
struct Gate {
    entered: mpsc::UnboundedSender<oneshot::Sender<()>>,
    hits: Arc<AtomicUsize>,
    release_all: CancellationToken,
}

async fn hold(State(gate): State<Gate>) -> StatusCode {
    gate.hits.fetch_add(1, Ordering::SeqCst);
    let (release, waiting) = oneshot::channel();
    if gate.entered.send(release).is_ok() {
        tokio::select! {
            _ = waiting => {},
            _ = gate.release_all.cancelled() => {},
        }
    }
    StatusCode::NO_CONTENT
}

async fn rows(access: &ConfigAccess, query: &str, collection: &str) -> Vec<Value> {
    let response = access.execute(query).await.unwrap();
    serde_json::from_value(response["data"][collection].clone()).unwrap()
}

async fn invocations(access: &ConfigAccess) -> Vec<Value> {
    rows(
        access,
        "{ CallbackInvocation(order: {invocation_id: ASC}) {
            invocation_id input lifecycle_state attempts action_journal error
        } }",
        "CallbackInvocation",
    )
    .await
}

async fn wait_for_invocations(
    access: &ConfigAccess,
    updates: &mut events::Subscription,
    ready: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    tokio::time::timeout(WAIT, async {
        loop {
            let current = invocations(access).await;
            if ready(&current) {
                return current;
            }
            updates
                .recv()
                .await
                .expect("database update stream stays open");
        }
    })
    .await
    .expect("callback invocations did not reach the expected durable state")
}

async fn wait_for_arrival_checkpoint(access: &ConfigAccess, updates: &mut events::Subscription) {
    tokio::time::timeout(WAIT, async {
        loop {
            let response = access.execute(
                "{ _documentArrivals(collection: \"SaturationInput\", after: \"0\", limit: 1) { head }
                    EventSourceCursor { after } }",
            ).await.unwrap();
            let data = &response["data"];
            let cursors = data["EventSourceCursor"].as_array().unwrap();
            assert_eq!(cursors.len(), 1);
            if cursors[0]["after"] == data["_documentArrivals"]["head"] {
                return;
            }
            updates.recv().await.expect("database update stream stays open");
        }
    }).await.expect("source delivery must checkpoint after the capacity refusal");
}

fn transition(name: &str) -> &'static Value {
    crate::lean_vocab_test::lean_contract_snapshot()
        .callback_transition_cases
        .iter()
        .find(|case| case["name"] == name)
        .expect("callback transition emitted by Lean")
}

fn assert_phase(row: &Value, phase: &Value, attempts: &Value) {
    assert_eq!(row["lifecycle_state"], phase["state"], "{row}");
    assert_eq!(row["attempts"], *attempts, "{row}");
    let journal: Value = serde_json::from_str(row["action_journal"].as_str().unwrap()).unwrap();
    assert_eq!(journal, phase["journal"], "{row}");
    assert!(row["error"].is_null(), "{row}");
}

async fn create_inputs(access: &ConfigAccess, items: impl Iterator<Item = usize>) {
    let input = Value::Array(
        items
            .map(|item| json!({"item": item, "correlation": format!("item-{item}")}))
            .collect(),
    );
    let literal = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    access
        .write(
            "test.callback_saturation_input",
            &format!("mutation {{ create_SaturationInput(input: {literal}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
}

async fn snapshot(node: &defra_node::EmbeddedNode, row: &Value) -> CallbackInvocationDoc {
    load_invocation(node, row["invocation_id"].as_str().unwrap(), OWNER)
        .await
        .unwrap()
        .unwrap()
}

async fn result_rows(access: &ConfigAccess) -> Vec<Value> {
    rows(
        access,
        "{ CallbackResult(order: {invocation_id: ASC}) { invocation_id caused_by_correlation } }",
        "CallbackResult",
    )
    .await
}

async fn output_rows(access: &ConfigAccess) -> Vec<Value> {
    rows(
        access,
        "{ SaturationOutput(order: {item: ASC}) { item correlation http_results } }",
        "SaturationOutput",
    )
    .await
}

/// HTTP gates must outlive this bounded scenario: the fixture declares a
/// thirty-second plugin budget, whose HTTP allowance also exceeds twenty
/// seconds after reserving the final round. Production budgets are unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_engine_recovers_saturated_plugin_without_new_arrivals() {
    let claim = transition("claim_preserves_captured_group");
    let running = &transition("interrupt_marks_executing_action_and_fails")["pre"];
    let success = &transition("success_preserves_captured_group_and_emits_result")["post"];
    assert_eq!(success["result_emitted"], true);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let release_all = CancellationToken::new();
    let hits = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route("/hold", get(hold)).with_state(Gate {
        entered,
        hits: hits.clone(),
        release_all: release_all.clone(),
    });
    let server_stop = CancellationToken::new();
    let server_shutdown = server_stop.clone();
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(server_shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let _server_guard = server_stop.clone().drop_guard();
    let _release_guard = release_all.clone().drop_guard();

    let canned = json!({"http_calls": {
        "requests": [{"id": "release", "url": format!("http://{address}/hold")}],
        "state": {"step": 1}
    }});
    let (home, mut plugin) = installed_plugin(
        &asking_plugin_wat("http_calls", "http_results", &canned, false),
        None,
    );
    let _identity =
        crate::request::lifecycle::test_support::pin_fixed_signing_identity(home.path());
    plugin.declaration.manifold = Some(json!({
        "fs": "None", "net": {"OutboundHttp": [format!("http://{address}")]},
        "env": "None", "crypto": false, "child_process": false
    }));
    plugin.declaration.limits = Some(crate::pack::PluginLimits {
        wall_clock_secs: Some(30),
        ..Default::default()
    });
    plugin.granted =
        store::grant_on_install(home.path(), &plugin.namespace, &plugin.declaration, true).unwrap();
    store::write_record(home.path(), &plugin).unwrap();
    let plugins = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    access
        .add_collection(
            "type SaturationInput { item: Int correlation: String }
             type SaturationOutput { item: Int correlation: String http_calls: Boolean
                 state: JSON http_results: JSON }",
        )
        .await
        .unwrap();
    let documents = [
        (
            crate::Collection::Callback,
            json!({"callback_id": CALLBACK, "handler": {
                "kind": "plugin", "plugin": "team/plugin", "digest": plugin.digest,
                "correlation_field": "correlation", "outputs": [{
                    "name": "result", "collection": "SaturationOutput",
                    "schema": "SaturationOutput/v1", "correlation_field": "correlation",
                    "cardinality": "one", "required": true
                }]
            }}),
        ),
        (
            crate::Collection::EventSource,
            json!({"event_source_id": "saturation", "source_collection": "SaturationInput",
                "event_kind": "created", "correlation_field": "correlation"}),
        ),
        (
            crate::Collection::CallbackBinding,
            json!({"binding_id": "saturation", "event_source_id": "saturation",
                "callback_id": CALLBACK, "input_fields": ["item", "correlation"]}),
        ),
    ]
    .into_iter()
    .map(|(collection, mut value)| {
        value["node_did"] = json!(OWNER);
        DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        }
    })
    .collect();
    let plan = DesiredStateApplyPlan::new(documents).unwrap();
    access
        .transact_write("test.callback_saturation_config", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .unwrap();
    create_inputs(&access, 0..PLUGIN_CALLBACK_CONCURRENCY).await;

    let cancel = CancellationToken::new();
    let _cancel_guard = cancel.clone().drop_guard();
    let mut engine = tokio::spawn(crate::triggers::callback::run_callback_engine(
        node.clone(),
        OWNER.into(),
        None,
        plugins.clone(),
        cancel.clone(),
    ));

    let scenario = async {
        let mut releases = Vec::new();
        for _ in 0..PLUGIN_CALLBACK_CONCURRENCY {
            releases.push(
                tokio::time::timeout(WAIT, arrivals.recv())
                    .await
                    .expect("all plugin workers must enter their HTTP gates")
                    .unwrap(),
            );
        }
        let initial = invocations(&access).await;
        assert_eq!(initial.len(), PLUGIN_CALLBACK_CONCURRENCY);
        for row in &initial {
            assert_phase(row, running, &claim["post"]["attempts"]);
        }
        assert!(result_rows(&access).await.is_empty());
        let stale_running = snapshot(&node, &initial[0]).await;

        let mut updates = node.subscribe_updates();
        create_inputs(
            &access,
            PLUGIN_CALLBACK_CONCURRENCY..PLUGIN_CALLBACK_CONCURRENCY + 1,
        )
        .await;
        let saturated = wait_for_invocations(&access, &mut updates, |rows| {
            rows.len() == PLUGIN_CALLBACK_CONCURRENCY + 1
        })
        .await;
        let pending = saturated
            .iter()
            .find(|row| row["input"]["item"] == PLUGIN_CALLBACK_CONCURRENCY)
            .unwrap();
        assert_phase(pending, &claim["pre"], &claim["pre"]["attempts"]);
        let stale_pending = snapshot(&node, pending).await;
        wait_for_arrival_checkpoint(&access, &mut updates).await;
        assert_eq!(
            snapshot(&node, pending).await.lifecycle_state,
            claim["pre"]["state"]
        );
        assert_eq!(hits.load(Ordering::SeqCst), PLUGIN_CALLBACK_CONCURRENCY);
        assert!(arrivals.try_recv().is_err());

        releases.pop().unwrap().send(()).unwrap();
        releases.push(tokio::time::timeout(WAIT, arrivals.recv()).await
            .expect("periodic recovery must start the pending invocation without another source arrival")
            .unwrap());
        let resumed = invocations(&access).await;
        let recovered = resumed
            .iter()
            .find(|row| row["invocation_id"] == pending["invocation_id"])
            .unwrap();
        assert_phase(recovered, running, &claim["post"]["attempts"]);
        let mut succeeded = 0;
        for row in &resumed {
            if row["lifecycle_state"] == success["state"] {
                succeeded += 1;
                assert_phase(row, success, &claim["post"]["attempts"]);
            } else {
                assert_phase(row, running, &claim["post"]["attempts"]);
            }
        }
        assert_eq!(
            succeeded, 1,
            "the sweep must leave all held attempts running"
        );
        assert_eq!(result_rows(&access).await.len(), 1);
        assert_eq!(hits.load(Ordering::SeqCst), PLUGIN_CALLBACK_CONCURRENCY + 1);
        updates = node.subscribe_updates();
        for release in releases {
            release.send(()).unwrap();
        }
        let completed = wait_for_invocations(&access, &mut updates, |rows| {
            rows.len() == PLUGIN_CALLBACK_CONCURRENCY + 1
                && rows
                    .iter()
                    .all(|row| row["lifecycle_state"] == success["state"])
        })
        .await;
        for row in &completed {
            assert_phase(row, success, &claim["post"]["attempts"]);
        }
        let results = result_rows(&access).await;
        assert_eq!(results.len(), completed.len());
        assert_eq!(
            results
                .iter()
                .map(|row| row["invocation_id"].as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            completed
                .iter()
                .map(|row| row["invocation_id"].as_str().unwrap())
                .collect::<BTreeSet<_>>()
        );
        let outputs = output_rows(&access).await;
        assert_eq!(outputs.len(), completed.len());
        for (item, output) in outputs.iter().enumerate() {
            assert_eq!(output["item"], item);
            assert_eq!(output["correlation"], format!("item-{item}"));
            assert_eq!(output["http_results"]["release"]["status"], 204, "{output}");
            assert_eq!(output["http_results"]["release"]["body"], "", "{output}");
        }
        let sources = rows(&access, "{ SaturationInput { _docID } }", "SaturationInput").await;
        assert_eq!(sources.len(), completed.len());

        let callback = load_callback(&node, CALLBACK, OWNER)
            .await
            .unwrap()
            .unwrap();
        let stale_workers = CallbackWorkers::new(1, CancellationToken::new());
        for stale in [stale_pending, stale_running] {
            stale_workers.dispatch(node.clone(), &stale, &callback, None, plugins.clone());
            stale_workers.drain().await;
        }
        assert_eq!(
            invocations(&access).await,
            completed,
            "stale dispatch must reload terminal state"
        );
        assert_eq!(result_rows(&access).await, results);
        assert_eq!(output_rows(&access).await, outputs);
        assert_eq!(hits.load(Ordering::SeqCst), completed.len());
        assert!(arrivals.try_recv().is_err());
    };
    let outcome = AssertUnwindSafe(async {
        tokio::time::timeout(SCENARIO_LIMIT, scenario)
            .await
            .expect("callback saturation scenario exceeded its total deadline");
    })
    .catch_unwind()
    .await;

    cancel.cancel();
    release_all.cancel();
    let drained = tokio::time::timeout(WAIT, &mut engine).await;
    if drained.is_err() {
        engine.abort();
    }
    server_stop.cancel();
    let stopped = tokio::time::timeout(WAIT, &mut server).await;
    if stopped.is_err() {
        server.abort();
    }
    node.shutdown().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    drained
        .expect("callback workers drain on cancellation")
        .unwrap()
        .unwrap();
    stopped.expect("HTTP gates release on cleanup").unwrap();
}
