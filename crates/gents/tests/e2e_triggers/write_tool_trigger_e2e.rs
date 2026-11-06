use std::time::Duration;

use gents::documents::{WriteToolDecl, WriteToolField};
use gents::inference::llm::tool::Tool;
use gents::store::graphql::escape_graphql_string;
use gents::tools::builtin::defra_write::BoundedWriteTool;
use gents::{Collection, ConfigAccess, DocumentRuntimeOptions, Gents, ToolCeiling};
use gents_protocol::row::AgentRequestRow;
use serde_json::{json, Value};

use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{
    fetch_runtime_snapshot_as, is_routed_ready_after, RuntimeSnapshot,
};
use crate::support::{managed_agent_fixture, test_db, ManagedAgentFixture};

const DRIFT_SIG: &str = "drift:host-doc:9f2c";
const SUMMARY: &str = "studio-1 host doc is stale vs runtime";
const TARGET_PATHS: &str = "infra/hosts/studio-1/host.md";
const PROMPT_TEMPLATE: &str =
    "drift={{ doc.drift_sig }} summary={{ doc.summary }} paths={{ doc.target_paths }}";

const ACTION_REQUEST_SDL: &str = r#"
    type ActionRequest {
        drift_sig: String
        summary: String
        target_paths: String
        status: String @index
    }
"#;

async fn register_action_request_schema(access: &ConfigAccess) {
    let plan = gents::store::config_client::preview_schema_install(access, ACTION_REQUEST_SDL)
        .await
        .expect("preview ActionRequest schema");
    gents::store::config_client::apply_schema_install(
        access,
        ACTION_REQUEST_SDL,
        &plan.artifact_digest,
    )
    .await
    .expect("install ActionRequest schema");
}

async fn apply_documents(access: &ConfigAccess, documents: Vec<(Collection, serde_json::Value)>) {
    use gents::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .unwrap();
    access
        .transact_write("test.automation_configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .unwrap();
}

async fn configure_action_trigger(
    access: &ConfigAccess,
    owner: &str,
    task_id: &str,
    trigger_id: &str,
    agent_did: &str,
    prompt_template: &str,
) {
    apply_documents(access, vec![
        (
            Collection::Task,
            serde_json::json!({"node_did":owner,"task_id":task_id,"agent_did":agent_did,"prompt_template":prompt_template}),
        ),
        (
            Collection::EventSource,
            serde_json::json!({"node_did":owner,"event_source_id":trigger_id,"source_collection":"ActionRequest","event_kind":"created"}),
        ),
        (
            Collection::Trigger,
            serde_json::json!({"node_did":owner,"trigger_id":trigger_id,"task_id":task_id,"source":{"kind":"event","event_source_id":trigger_id},"concurrency":"serial"}),
        ),
    ])
    .await;
}

async fn wait_for_runtime_snapshot<F>(
    access: &ConfigAccess,
    node_did: &str,
    runtime: &tokio::task::JoinHandle<anyhow::Result<()>>,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let last_snapshot = fetch_runtime_snapshot_as(access, node_did).await;
        if let Some(snapshot) = last_snapshot.as_ref() {
            if predicate(snapshot) {
                return snapshot.clone();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime snapshot for {node_did}; runtime_finished={}; \
             last_snapshot={last_snapshot:?}",
            runtime.is_finished()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[test]
fn routed_readiness_uses_generation_not_transient_reconcile_label() {
    let snapshot = RuntimeSnapshot {
        process_state: "ready".into(),
        reconcile_phase: "idle".into(),
        active_generation: 2,
        router_generation: 2,
        default_agent_did: "behavior".into(),
        last_reconcile_result: "noop".into(),
        last_reconcile_error: String::new(),
    };
    assert!(is_routed_ready_after(&snapshot, 1));
    let mut applied = snapshot.clone();
    applied.last_reconcile_result = "applied".into();
    assert!(is_routed_ready_after(&applied, 1));
    assert!(!is_routed_ready_after(&snapshot, 2));

    let mut starting = snapshot.clone();
    starting.process_state = "starting".into();
    assert!(!is_routed_ready_after(&starting, 1));

    let mut not_routed = snapshot.clone();
    not_routed.router_generation = 1;
    assert!(!is_routed_ready_after(&not_routed, 1));

    let mut reconciling = snapshot.clone();
    reconciling.reconcile_phase = "applying".into();
    assert!(!is_routed_ready_after(&reconciling, 1));

    let mut failed = snapshot;
    failed.last_reconcile_error = "reconcile failed".into();
    assert!(!is_routed_ready_after(&failed, 1));
}

async fn query_agent_requests_for_trigger(
    access: &ConfigAccess,
    trigger_id: &str,
) -> Vec<AgentRequestRow> {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_trigger_id: {{ _eq: "{escaped_trigger_id}" }} }}
            ) {{
                request_id
                content
                caused_by_trigger_id
                caused_by_trigger_kind
                execution_origin
            }}
        }}"#
    );
    let response = access
        .execute(&query)
        .await
        .expect("read triggered AgentRequest");
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| serde_json::from_value(row).expect("decode AgentRequest row"))
        .collect()
}

async fn wait_for_one_agent_request(access: &ConfigAccess, trigger_id: &str) -> AgentRequestRow {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut requests = loop {
        let rows = query_agent_requests_for_trigger(access, trigger_id).await;
        if !rows.is_empty() {
            break rows;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest caused_by_trigger_id={trigger_id}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one AgentRequest from a single matching source doc, got {}: {:?}",
        requests.len(),
        requests
    );
    requests.remove(0)
}

fn assert_multi_field_render(request: &AgentRequestRow, trigger_id: &str) {
    assert_eq!(
        request.caused_by_trigger_id.as_deref(),
        Some(trigger_id),
        "request caused_by_trigger_id mismatch: {request:?}"
    );
    assert_eq!(
        request.caused_by_trigger_kind.as_deref(),
        Some("event"),
        "request caused_by_trigger_kind must be 'event': {request:?}"
    );
    assert_eq!(
        request.execution_origin.as_deref(),
        Some("scheduled"),
        "trigger-driven fires persist execution_origin=scheduled: {request:?}"
    );
    let expected = format!("]\n\ndrift={DRIFT_SIG} summary={SUMMARY} paths={TARGET_PATHS}");
    assert!(
        request.content.as_deref().is_some_and(|content| content
            .starts_with(&format!("[event trigger:{trigger_id} · "))
            && content.ends_with(&expected)),
        "rendered prompt must substitute ALL of drift_sig/summary/target_paths from the \
         triggering doc: {request:?}"
    );
    assert!(
        request
            .content
            .as_deref()
            .is_some_and(|content| content.contains(DRIFT_SIG)),
        "render missing drift_sig: {request:?}"
    );
    assert!(
        request
            .content
            .as_deref()
            .is_some_and(|content| content.contains(SUMMARY)),
        "render missing summary: {request:?}"
    );
    assert!(
        request
            .content
            .as_deref()
            .is_some_and(|content| content.contains(TARGET_PATHS)),
        "render missing target_paths: {request:?}"
    );
    assert!(
        !request.request_id.is_empty(),
        "request_id must be populated: {request:?}"
    );
}

async fn boot_agent_with_action_trigger(
    db: &crate::support::TestDb,
    fixture: &ManagedAgentFixture,
    test_name: &str,
    task_id: &str,
    trigger_id: &str,
) -> (
    tokio::task::JoinHandle<anyhow::Result<()>>,
    tokio::sync::watch::Sender<bool>,
) {
    let identity = db.node_identity.clone();
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_managed_agent_backend(
        db,
        fixture,
        &format!("backend-{test_name}"),
        mock_endpoint.endpoint(),
        "default",
    )
    .await;

    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let node_did = agent.node_did().to_string();
    let default_agent_did = agent.default_agent_did().to_string();

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));

    let startup = wait_for_runtime_snapshot(&fixture.access, &node_did, &handle, |snapshot| {
        is_routed_ready_after(snapshot, 0)
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    configure_action_trigger(
        &fixture.access,
        &node_did,
        task_id,
        trigger_id,
        &default_agent_did,
        PROMPT_TEMPLATE,
    )
    .await;

    let reconciled = wait_for_runtime_snapshot(&fixture.access, &node_did, &handle, |snapshot| {
        is_routed_ready_after(snapshot, initial_generation)
    })
    .await;
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "post-insert reconcile should be clean, got error={:?}",
        reconciled.last_reconcile_error
    );

    (handle, shutdown_tx)
}

fn request_action_decl() -> WriteToolDecl {
    WriteToolDecl {
        notification: None,
        tool_name: "request_action".into(),
        collection: "ActionRequest".into(),
        description: "Emit one ActionRequest describing observed drift.".into(),
        fields: vec![
            WriteToolField {
                name: "drift_sig".into(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "summary".into(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "target_paths".into(),
                required: true,
                fill: None,
            },
            WriteToolField {
                name: "status".into(),
                required: false,
                fill: None,
            },
        ],
        output_obligation: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_field_template_renders_all_referenced_doc_fields() {
    let _guard = crate::P2P_E2E_LOCK.lock().await;
    let db = test_db("write-tool-trigger-multifield").await;
    let fixture = managed_agent_fixture(&db).await;
    register_action_request_schema(&fixture.access).await;

    let task_id = "task-b6-multifield";
    let trigger_id = "trigger-b6-multifield";
    let (handle, shutdown_tx) =
        boot_agent_with_action_trigger(&db, &fixture, "b6-multifield", task_id, trigger_id).await;

    gents::store::config_client::create_authored_document(
        &fixture.access,
        "ActionRequest",
        &json!({"drift_sig": DRIFT_SIG, "summary": SUMMARY, "target_paths": TARGET_PATHS, "status": "open"}),
        gents::store::config_client::DocumentSchemaSource::UserAuthored,
        ACTION_REQUEST_SDL,
    ).await.expect("create User-signed ActionRequest");

    let request = wait_for_one_agent_request(&fixture.access, trigger_id).await;
    assert_multi_field_render(&request, trigger_id);

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_write_tool_call_fires_event_trigger() {
    let _guard = crate::P2P_E2E_LOCK.lock().await;
    let db = test_db("write-tool-trigger-tooldriven").await;
    let fixture = managed_agent_fixture(&db).await;
    register_action_request_schema(&fixture.access).await;

    let task_id = "task-b6-tooldriven";
    let trigger_id = "trigger-b6-tooldriven";
    let (handle, shutdown_tx) =
        boot_agent_with_action_trigger(&db, &fixture, "b6-tooldriven", task_id, trigger_id).await;

    let tool = BoundedWriteTool::new(
        gents::ConfigAccess::local_managed_agent(
            db.node.clone(),
            ::identity::Did::new(fixture.agent.did()).unwrap(),
            fixture.agent.as_ref(),
        )
        .unwrap(),
        request_action_decl(),
    );
    assert_eq!(
        Tool::name(&tool),
        "request_action",
        "tool advertises its declared name"
    );
    let out = Tool::call(
        &tool,
        serde_json::from_value(json!({
            "drift_sig": DRIFT_SIG,
            "summary": SUMMARY,
            "target_paths": TARGET_PATHS,
            "status": "open",
        }))
        .expect("decode write-tool args"),
    )
    .await
    .expect("request_action write tool call");
    assert!(
        out.contains("ActionRequest"),
        "tool should report the ActionRequest it created: {out}"
    );

    let action_rows = fixture
        .access
        .execute("{ ActionRequest { drift_sig summary target_paths status } }")
        .await
        .expect("read ActionRequest rows");
    let rows = action_rows["data"]["ActionRequest"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        rows.len(),
        1,
        "write tool must have written exactly one ActionRequest doc: {rows:?}"
    );
    assert_eq!(
        rows[0].get("drift_sig").and_then(Value::as_str),
        Some(DRIFT_SIG)
    );
    assert_eq!(
        rows[0].get("summary").and_then(Value::as_str),
        Some(SUMMARY)
    );
    assert_eq!(
        rows[0].get("target_paths").and_then(Value::as_str),
        Some(TARGET_PATHS)
    );

    let request = wait_for_one_agent_request(&fixture.access, trigger_id).await;
    assert_multi_field_render(&request, trigger_id);

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
