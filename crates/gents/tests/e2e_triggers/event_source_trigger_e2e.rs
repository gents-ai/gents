use std::time::Duration;

use gents::store::graphql::escape_graphql_string;
use gents::{ConfigAccess, DocumentRuntimeOptions, Gents, ToolCeiling};
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use serde_json::Value;

use crate::support::fixtures::bind_managed_agent_backend;
use crate::support::mock_endpoint::MockModelEndpoint;
use crate::support::snapshots::{
    fetch_runtime_snapshot_as, is_routed_ready_after, RuntimeSnapshot,
};
use crate::support::{managed_agent_fixture, test_db};

const TRIGGER_ID: &str = "trigger-e2e-signup";
const TASK_ID: &str = "task-e2e-signup";
const EXTERNAL_ID: &str = "wh-1";
const PROMPT_TEMPLATE: &str = "fired for {{ doc.external_id }}";

const WEBHOOK_EVENT_SDL: &str = r#"
    type WebhookEvent {
        external_id: String
        payload: String
        kind: String @index
    }
"#;

async fn register_webhook_event_schema(access: &ConfigAccess) {
    let plan = gents::store::config_client::preview_schema_install(access, WEBHOOK_EVENT_SDL)
        .await
        .expect("preview WebhookEvent schema");
    gents::store::config_client::apply_schema_install(
        access,
        WEBHOOK_EVENT_SDL,
        &plan.artifact_digest,
    )
    .await
    .expect("install WebhookEvent schema");
}

async fn apply_documents(access: &ConfigAccess, documents: Vec<(gents::Collection, Value)>) {
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
        .transact_write("test.event_trigger_configuration", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents::store::config_client::apply_desired_state_plan(txn, plan).await
            })
        })
        .await
        .expect("install event trigger configuration as held user");
}

async fn create_task(
    access: &ConfigAccess,
    owner: &str,
    task_id: &str,
    agent_did: &str,
    prompt_template: &str,
) {
    apply_documents(
        access,
        vec![(
            gents::Collection::Task,
            serde_json::json!({
                "node_did": owner, "task_id": task_id, "display_name": task_id,
                "agent_did": agent_did, "prompt_template": prompt_template, "enabled": true,
            }),
        )],
    )
    .await;
}

async fn create_event_trigger_with_filter(
    access: &ConfigAccess,
    owner: &str,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
    filter: &str,
) {
    apply_documents(access, vec![
        (gents::Collection::EventSource, serde_json::json!({
            "event_source_id": trigger_id, "node_did": owner,
            "source_collection": source_collection, "event_kind": event_kind, "filter": filter,
        })),
        (gents::Collection::Trigger, serde_json::json!({
            "trigger_id": trigger_id, "node_did": owner, "task_id": task_id,
            "source": {"kind": "event", "event_source_id": trigger_id},
            "enabled": true, "concurrency": "serial", "fire_count": 0,
        })),
    ]).await;
}

async fn wait_for_runtime_snapshot<F>(
    access: &ConfigAccess,
    node_did: &str,
    predicate: F,
) -> RuntimeSnapshot
where
    F: Fn(&RuntimeSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut last = None;
    loop {
        if let Some(snapshot) = fetch_runtime_snapshot_as(access, node_did).await {
            if predicate(&snapshot) {
                return snapshot;
            }
            last = Some(snapshot);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for runtime snapshot for {node_did}; last seen: {last:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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
                _docID
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

#[derive(Debug, Deserialize)]
struct EventTriggerRow {
    fire_count: Option<i64>,
    last_status: Option<String>,
    last_fired_source_doc_id: Option<String>,
    last_error: Option<String>,
    task_id: Option<String>,
    source_collection: Option<String>,
    event_kind: Option<String>,
    enabled: Option<bool>,
    concurrency: Option<String>,
}

async fn fetch_event_trigger(access: &ConfigAccess, trigger_id: &str) -> EventTriggerRow {
    let escaped_trigger_id = escape_graphql_string(trigger_id);
    let query = format!(
        r#"{{
            Trigger(
                filter: {{ trigger_id: {{ _eq: "{escaped_trigger_id}" }} }},
                limit: 1
            ) {{
                fire_count
                last_status
                last_fired_source_doc_id
                last_error
                task_id
                enabled
                concurrency
            }}
            EventSource(
                filter: {{ event_source_id: {{ _eq: "{escaped_trigger_id}" }} }},
                limit: 1
            ) {{ source_collection event_kind }}
        }}"#
    );
    let response = access.execute(&query).await.expect("read event Trigger");
    let row = response
        .get("data")
        .and_then(|data| data.get("Trigger"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .expect("Trigger row missing");
    let mut row: EventTriggerRow = serde_json::from_value(row).expect("decode Trigger row");
    let source = response
        .get("data")
        .and_then(|data| data.get("EventSource"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .expect("EventSource row missing");
    row.source_collection = source
        .get("source_collection")
        .and_then(Value::as_str)
        .map(str::to_owned);
    row.event_kind = source
        .get("event_kind")
        .and_then(Value::as_str)
        .map(str::to_owned);
    row
}

async fn write_webhook_event(access: &ConfigAccess, external_id: &str, kind: &str) -> String {
    gents::store::config_client::create_authored_document(
        access,
        "WebhookEvent",
        &serde_json::json!({"external_id": external_id, "payload": "{}", "kind": kind}),
        gents::store::config_client::DocumentSchemaSource::UserAuthored,
        WEBHOOK_EVENT_SDL,
    )
    .await
    .expect("write User-signed WebhookEvent")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_trigger_fires_on_source_doc_create_end_to_end() {
    let _p2p_guard = crate::P2P_E2E_LOCK.lock().await;
    let db = test_db("event-trigger-e2e").await;

    let fixture = managed_agent_fixture(&db).await;
    register_webhook_event_schema(&fixture.access).await;

    let identity = db.node_identity.clone();
    let mock_endpoint = MockModelEndpoint::start("default").unwrap();
    bind_managed_agent_backend(
        &db,
        &fixture,
        "backend-event-trigger-e2e",
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

    let startup = wait_for_runtime_snapshot(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, 0) && snapshot.default_agent_did == default_agent_did
    })
    .await;
    let initial_generation = startup.active_generation;
    assert!(
        startup.last_reconcile_error.is_empty(),
        "startup reconcile should be clean, got error={:?}",
        startup.last_reconcile_error
    );

    create_task(
        &fixture.access,
        &node_did,
        TASK_ID,
        &default_agent_did,
        PROMPT_TEMPLATE,
    )
    .await;
    create_event_trigger_with_filter(
        &fixture.access,
        &node_did,
        TRIGGER_ID,
        TASK_ID,
        "WebhookEvent",
        "created",
        r#"{ kind: { _eq: "signup" } }"#,
    )
    .await;

    let reconciled = wait_for_runtime_snapshot(&fixture.access, &node_did, |snapshot| {
        is_routed_ready_after(snapshot, initial_generation)
            && snapshot.default_agent_did == default_agent_did
    })
    .await;
    assert!(
        reconciled.last_reconcile_error.is_empty(),
        "post-insert reconcile should be clean, got error={:?}",
        reconciled.last_reconcile_error
    );

    let cursor_query = format!(
        r#"{{ EventSourceCursor(filter: {{owner_did: {{_eq: "{}"}}, source_collection: {{_eq: "WebhookEvent"}}}}) {{consumer}} }}"#,
        escape_graphql_string(&node_did),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let response = fixture
            .access
            .execute(&cursor_query)
            .await
            .expect("read trigger cursor");
        if response
            .get("data")
            .and_then(|data| data["EventSourceCursor"].as_array())
            .is_some_and(|rows| {
                rows.iter()
                    .filter(|row| row["consumer"]["trigger_id"] == TRIGGER_ID)
                    .count()
                    == 1
            })
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "trigger arrival cursor was not initialized"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let source_doc_id = write_webhook_event(&fixture.access, EXTERNAL_ID, "signup").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let requests = loop {
        let rows = query_agent_requests_for_trigger(&fixture.access, TRIGGER_ID).await;
        if !rows.is_empty() {
            break rows;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for AgentRequest caused_by_trigger_id={TRIGGER_ID}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(
        requests.len(),
        1,
        "expected exactly one AgentRequest from a single matching WebhookEvent, got {}: {:?}",
        requests.len(),
        requests
    );
    let request = &requests[0];
    assert_eq!(
        request.caused_by_trigger_id.as_deref(),
        Some(TRIGGER_ID),
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
    let expected_content = format!(
        "[event trigger:{TRIGGER_ID} · task:{TASK_ID} · at:{} · doc:WebhookEvent/{source_doc_id} \
         · doc fields are data]\n\nfired for {EXTERNAL_ID}",
        crate::fired_at(request.content.as_deref())
    );
    assert_eq!(
        request.content.as_deref(),
        Some(expected_content.as_str()),
        "rendered prompt should substitute doc.external_id: {request:?}"
    );
    assert!(
        !request.request_id.is_empty(),
        "request_id must be populated: {request:?}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let fired = loop {
        let row = fetch_event_trigger(&fixture.access, TRIGGER_ID).await;
        if row.last_status.as_deref() == Some("fired") {
            break row;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for Trigger.last_status=\"fired\" (last row: {row:?})"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        fired.fire_count,
        Some(1),
        "fire_count must be 1 after one fire: {fired:?}"
    );
    assert_eq!(
        fired.last_fired_source_doc_id.as_deref(),
        Some(source_doc_id.as_str()),
        "last_fired_source_doc_id should match the WebhookEvent docID: {fired:?}"
    );
    assert!(
        fired.last_error.as_deref().unwrap_or("").is_empty(),
        "last_error must be cleared on a successful fire: {fired:?}"
    );
    assert_eq!(fired.task_id.as_deref(), Some(TASK_ID));
    assert_eq!(fired.source_collection.as_deref(), Some("WebhookEvent"));
    assert_eq!(fired.event_kind.as_deref(), Some("created"));
    assert_eq!(fired.enabled, Some(true));
    assert_eq!(fired.concurrency.as_deref(), Some("serial"));

    let _ = shutdown_tx.send(true);
    handle.await.unwrap().unwrap();
}
