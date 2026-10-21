use defra_node::EmbeddedNode;
use gents_migration::{
    ensure_migrations_with_registry, OrdinaryIndex, Registry, DEFAULT_BASELINE,
    DEFAULT_ORDINARY_INDEXES,
};
use gents_protocol::schemas::{AGENT_REQUEST, AGENT_REQUEST_NAME, MAILBOX_ITEM, MAILBOX_ITEM_NAME};
use serde_json::Value;

mod common;
use common::fresh_node;

fn created_request_doc_id(data: &Value) -> String {
    let value = data
        .get("created")
        .expect("aliased create_AgentRequest response");
    let row = value
        .as_array()
        .and_then(|rows| rows.first())
        .unwrap_or(value);
    row["_docID"]
        .as_str()
        .expect("created physical document ID")
        .to_owned()
}

fn scan_metric(value: &Value, name: &str) -> Option<u64> {
    match value {
        Value::Object(fields) => fields
            .get(name)
            .and_then(Value::as_u64)
            .or_else(|| fields.values().find_map(|value| scan_metric(value, name))),
        Value::Array(values) => values.iter().find_map(|value| scan_metric(value, name)),
        _ => None,
    }
}

async fn assert_indexed_query(node: &EmbeddedNode, query: &str, index: &str, max_fetches: u64) {
    let plan = node
        .execute(&query.replacen("query", "query @explain", 1))
        .await;
    assert!(!plan.has_errors(), "query plan: {:?}", plan.errors);
    assert!(
        plan.data.unwrap().to_string().contains(index),
        "query should use {index}"
    );
    let executed = node
        .execute(&query.replacen("query", "query @explain(type: execute)", 1))
        .await;
    assert!(
        !executed.has_errors(),
        "executed plan: {:?}",
        executed.errors
    );
    let plan = executed.data.unwrap();
    for metric in ["docFetches", "indexFetches"] {
        let count = scan_metric(&plan, metric).expect("executed scan metric");
        assert!(
            count <= max_fetches,
            "bounded read exceeded {max_fetches}: {metric}={count}: {plan}"
        );
    }
}

fn registry_with_request_index<'a>(indexes: &'a [OrdinaryIndex<'static>]) -> Registry<'a> {
    let request = DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == AGENT_REQUEST_NAME)
        .expect("AgentRequest baseline");
    Registry {
        baseline: std::slice::from_ref(request),
        steps: &[],
        ordinary_indexes: indexes,
    }
}

fn registry_with_mailbox_cursor_index() -> Registry<'static> {
    let mailbox = DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == MAILBOX_ITEM_NAME)
        .expect("MailboxItem baseline");
    let index = DEFAULT_ORDINARY_INDEXES
        .iter()
        .find(|index| index.name == "MailboxItem_node_status_item_key")
        .expect("MailboxItem cursor index");
    Registry {
        baseline: std::slice::from_ref(mailbox),
        steps: &[],
        ordinary_indexes: std::slice::from_ref(index),
    }
}

async fn create_request(node: &EmbeddedNode, id: &str, session: &str, content: &str) -> String {
    let id = gents_protocol::graphql::escape_graphql_string(id);
    let session = gents_protocol::graphql::escape_graphql_string(session);
    let content = gents_protocol::graphql::escape_graphql_string(content);
    let created = node
        .execute(&format!(
            r#"mutation {{ created: create_AgentRequest(input: {{
                request_id: "{id}"
                node_did: "did:key:node-a"
                requester_did: "did:key:requester"
                session_id: "{session}"
                lifecycle_state: "pending"
                content: "{content}"
            }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !created.has_errors(),
        "create request: {:?}",
        created.errors
    );
    created_request_doc_id(&created.data.expect("create response data"))
}

async fn create_trigger_request(node: &EmbeddedNode, id: &str, created_at: &str) -> String {
    let id = gents_protocol::graphql::escape_graphql_string(id);
    let created_at = gents_protocol::graphql::escape_graphql_string(created_at);
    let created = node
        .execute(&format!(
            r#"mutation {{ created: create_AgentRequest(input: {{
                request_id: "{id}"
                node_did: "did:key:node-a"
                session_id: "task-session"
                caused_by_trigger_id: "trigger-a"
                purpose: "normal"
                created_at: "{created_at}"
                lifecycle_state: "completed"
            }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !created.has_errors(),
        "create trigger request: {:?}",
        created.errors
    );
    created_request_doc_id(&created.data.expect("create response data"))
}

async fn create_retry_request(node: &EmbeddedNode, id: &str, retry_parent: &str) -> String {
    let id = gents_protocol::graphql::escape_graphql_string(id);
    let retry_parent = gents_protocol::graphql::escape_graphql_string(retry_parent);
    let created = node
        .execute(&format!(
            r#"mutation {{ created: create_AgentRequest(input: {{
                request_id: "{id}"
                node_did: "did:key:node-a"
                requester_did: "did:key:requester"
                session_id: "session-a"
                retry_parent_request: "{retry_parent}"
                lifecycle_state: "pending"
            }}) {{ _docID }} }}"#
        ))
        .await;
    assert!(
        !created.has_errors(),
        "create retry request: {:?}",
        created.errors
    );
    created_request_doc_id(&created.data.expect("create response data"))
}

async fn request_rows(node: &EmbeddedNode) -> Value {
    let response = node
        .execute("query { AgentRequest { _docID request_id node_did session_id lifecycle_state } }")
        .await;
    assert!(
        !response.has_errors(),
        "read requests: {:?}",
        response.errors
    );
    let mut rows = response.data.unwrap()["AgentRequest"]
        .as_array()
        .expect("AgentRequest array")
        .clone();
    rows.sort_by(|left, right| left["_docID"].as_str().cmp(&right["_docID"].as_str()));
    Value::Array(rows)
}

fn request_index(collection: &defra_node::CollectionVersion) -> Value {
    let index = collection
        .indexes
        .iter()
        .find(|index| index.name == "AgentRequest_node_did_session_id")
        .expect("reconciled AgentRequest index");
    serde_json::json!({
        "name": index.name,
        "fields": index.fields.iter().map(|field| (&field.name, field.descending)).collect::<Vec<_>>(),
    })
}

fn trigger_history_index(collection: &defra_node::CollectionVersion) -> Value {
    let index = collection
        .indexes
        .iter()
        .find(|index| index.name == "AgentRequest_node_trigger_created_request")
        .expect("reconciled AgentRequest trigger history index");
    serde_json::json!({
        "name": index.name,
        "fields": index.fields.iter().map(|field| (&field.name, field.descending)).collect::<Vec<_>>(),
    })
}

fn index_inventory(collection: &defra_node::CollectionVersion) -> Vec<Value> {
    let mut indexes = collection
        .indexes
        .iter()
        .map(|index| {
            serde_json::json!({
                "name": index.name,
                "fields": index.fields.iter().map(|field| (&field.name, field.descending)).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    indexes.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    indexes
}

#[tokio::test]
async fn existing_and_fresh_stores_reconcile_the_same_index_without_changing_documents() {
    let indexes = DEFAULT_ORDINARY_INDEXES
        .iter()
        .copied()
        .filter(|index| index.collection == AGENT_REQUEST_NAME)
        .collect::<Vec<_>>();
    let registry = registry_with_request_index(&indexes);
    let existing = fresh_node().await;
    existing
        .add_schema(AGENT_REQUEST)
        .await
        .expect("register pre-index AgentRequest schema");
    let first_doc = create_request(existing.as_ref(), "request-twin", "session-a", "first").await;
    let second_doc = create_request(existing.as_ref(), "request-twin", "session-a", "second").await;
    let before_collection = existing
        .get_collection(AGENT_REQUEST_NAME)
        .expect("load pre-index collection")
        .expect("AgentRequest collection exists");
    assert!(!before_collection
        .indexes
        .iter()
        .any(|index| index.name == "AgentRequest_node_did_session_id"));
    let before_rows = request_rows(existing.as_ref()).await;

    ensure_migrations_with_registry(existing.as_ref(), &registry)
        .await
        .expect("reconcile existing index");
    let after_first_reconciliation = existing
        .get_collection(AGENT_REQUEST_NAME)
        .expect("load reconciled collection")
        .expect("AgentRequest collection exists");
    let first_definition = request_index(&after_first_reconciliation);
    for previous in &before_collection.indexes {
        assert!(
            after_first_reconciliation
                .indexes
                .iter()
                .any(|current| current.clone().normalized() == previous.clone().normalized()),
            "existing index changed during reconciliation: {previous:?}"
        );
    }

    ensure_migrations_with_registry(existing.as_ref(), &registry)
        .await
        .expect("repeat reconciliation");
    let after_repeat = existing
        .get_collection(AGENT_REQUEST_NAME)
        .expect("load collection after repeat")
        .expect("AgentRequest collection exists");
    assert_eq!(request_index(&after_repeat), first_definition);
    assert_eq!(before_collection.version_id, after_repeat.version_id);
    assert_eq!(before_collection.collection_id, after_repeat.collection_id);
    assert_eq!(request_rows(existing.as_ref()).await, before_rows);
    let mut expected_docs = [first_doc.as_str(), second_doc.as_str()];
    expected_docs.sort_unstable();
    assert_eq!(
        before_rows
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["_docID"].as_str().unwrap())
            .collect::<Vec<_>>(),
        expected_docs
    );

    let mut history_docs = Vec::new();
    for index in 0..64 {
        history_docs.push(
            create_trigger_request(
                existing.as_ref(),
                &format!("historic-{index:03}"),
                "2025-01-01T00:00:00Z",
            )
            .await,
        );
    }
    let older_task = create_trigger_request(
        existing.as_ref(),
        "task-request-older",
        "2026-01-01T00:00:00Z",
    )
    .await;
    let newer_task = create_trigger_request(
        existing.as_ref(),
        "task-request-newer",
        "2026-01-02T00:00:00Z",
    )
    .await;
    for index in 0..16 {
        let created = existing
            .execute(&format!(
                r#"mutation {{
            create_AgentRequest(input: {{ request_id: "legacy-{index}",
                node_did: "did:key:node-a", session_id: "task-session",
                caused_by_trigger_id: "trigger-a", purpose: null,
                created_at: "2030-01-01T00:00:00Z", lifecycle_state: "completed"
            }}) {{ _docID }}
        }}"#
            ))
            .await;
        assert!(
            !created.has_errors(),
            "legacy fixture: {:?}",
            created.errors
        );
    }
    let trigger_query = r#"query {
        AgentRequest(
            filter: { node_did: { _eq: "did:key:node-a" }, caused_by_trigger_id: { _eq: "trigger-a" }, purpose: { _eq: "normal" } },
            order: [{ node_did: ASC }, { caused_by_trigger_id: ASC }, { purpose: ASC }, { created_at: DESC }, { request_id: DESC }, { session_id: ASC }], limit: 8
        ) { _docID request_id }
    }"#;
    let trigger_rows = existing.execute(trigger_query).await;
    assert!(
        !trigger_rows.has_errors(),
        "trigger read: {:?}",
        trigger_rows.errors
    );
    let trigger_data = trigger_rows.data.unwrap();
    let trigger_rows = trigger_data["AgentRequest"]
        .as_array()
        .expect("trigger request rows");
    assert_eq!(trigger_rows.len(), 8);
    assert_eq!(trigger_rows[0]["_docID"], newer_task);
    assert_eq!(trigger_rows[1]["_docID"], older_task);
    for (row, expected) in trigger_rows[2..].iter().zip(history_docs.iter().rev()) {
        assert_eq!(row["_docID"], *expected);
    }
    assert_indexed_query(
        existing.as_ref(),
        trigger_query,
        "AgentRequest_node_trigger_created_request",
        9,
    )
    .await;

    let retry_doc = create_retry_request(existing.as_ref(), "retry-child", "request-twin").await;
    let predecessor_query = r#"query {
        AgentRequest(filter: {
            request_id: { _eq: "request-twin" }, node_did: { _eq: "did:key:node-a" },
            session_id: { _eq: "session-a" }, requester_did: { _eq: "did:key:requester" }
        }) { _docID }
    }"#;
    let predecessor_rows = existing.execute(predecessor_query).await;
    assert!(
        !predecessor_rows.has_errors(),
        "predecessor read: {:?}",
        predecessor_rows.errors
    );
    assert_eq!(
        predecessor_rows.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_indexed_query(
        existing.as_ref(),
        predecessor_query,
        "AgentRequest_request_node_session_requester",
        2,
    )
    .await;

    let retry_query = r#"query {
        AgentRequest(filter: {
            retry_parent_request: { _eq: "request-twin" }, node_did: { _eq: "did:key:node-a" },
            session_id: { _eq: "session-a" }, requester_did: { _eq: "did:key:requester" }
        }) { _docID }
    }"#;
    let retry_rows = existing.execute(retry_query).await;
    assert!(
        !retry_rows.has_errors(),
        "retry read: {:?}",
        retry_rows.errors
    );
    assert_eq!(
        retry_rows.data.unwrap()["AgentRequest"],
        serde_json::json!([{"_docID": retry_doc}])
    );
    assert_indexed_query(
        existing.as_ref(),
        retry_query,
        "AgentRequest_retry_parent_node_session_requester",
        1,
    )
    .await;

    let excluded_doc = gents_protocol::graphql::escape_graphql_string(&first_doc);
    let filtered_query = format!(
        r#"query {{ AgentRequest(filter: {{ node_did: {{ _eq: "did:key:node-a" }}, session_id: {{ _eq: "session-a" }}, _docID: {{ _neq: "{excluded_doc}" }} }}) {{ _docID }} }}"#
    );
    let filtered = existing.execute(&filtered_query).await;
    assert!(
        !filtered.has_errors(),
        "filtered read: {:?}",
        filtered.errors
    );
    let mut filtered_docs = filtered.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["_docID"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    filtered_docs.sort();
    let mut expected = vec![second_doc, retry_doc];
    expected.sort();
    assert_eq!(
        filtered_docs, expected,
        "exclude only the specified physical document"
    );
    assert_indexed_query(
        existing.as_ref(),
        &filtered_query,
        "AgentRequest_node_did_session_id",
        3,
    )
    .await;

    let fresh = fresh_node().await;
    ensure_migrations_with_registry(fresh.as_ref(), &registry)
        .await
        .expect("fresh baseline and index");
    let fresh_collection = fresh
        .get_collection(AGENT_REQUEST_NAME)
        .expect("load fresh collection")
        .expect("AgentRequest collection exists");
    assert_eq!(after_repeat.version_id, fresh_collection.version_id);
    assert_eq!(after_repeat.collection_id, fresh_collection.collection_id);
    assert_eq!(request_index(&fresh_collection), first_definition);
    assert_eq!(
        trigger_history_index(&fresh_collection),
        trigger_history_index(&after_repeat)
    );
    assert_eq!(
        index_inventory(&fresh_collection),
        index_inventory(&after_repeat),
        "fresh and upgraded collections have the same complete index inventory"
    );

    existing.shutdown().await;
    fresh.shutdown().await;
}

#[tokio::test]
async fn mailbox_cursor_index_reconciles_on_an_existing_collection() {
    let node = fresh_node().await;
    node.add_schema(MAILBOX_ITEM)
        .await
        .expect("register pre-index MailboxItem schema");
    let before = node
        .get_collection(MAILBOX_ITEM_NAME)
        .expect("load pre-index MailboxItem collection")
        .expect("MailboxItem collection exists");
    assert!(!before
        .indexes
        .iter()
        .any(|index| index.name == "MailboxItem_node_status_item_key"));

    let registry = registry_with_mailbox_cursor_index();
    ensure_migrations_with_registry(node.as_ref(), &registry)
        .await
        .expect("reconcile cursor index on existing MailboxItem");
    let after = node
        .get_collection(MAILBOX_ITEM_NAME)
        .expect("load indexed MailboxItem collection")
        .expect("MailboxItem collection exists");
    assert_eq!(before.version_id, after.version_id);
    assert_eq!(before.collection_id, after.collection_id);
    let index = after
        .indexes
        .iter()
        .find(|index| index.name == "MailboxItem_node_status_item_key")
        .expect("reconciled MailboxItem cursor index");
    assert_eq!(
        index
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.descending))
            .collect::<Vec<_>>(),
        vec![("node_did", false), ("status", false), ("item_key", false)]
    );

    ensure_migrations_with_registry(node.as_ref(), &registry)
        .await
        .expect("reconcile MailboxItem cursor index idempotently");
    node.shutdown().await;
}

#[tokio::test]
async fn output_source_index_backfills_existing_rows_and_retains_conflicts() {
    use gents_protocol::schemas::{AGENT_OUTPUT_SEGMENT, AGENT_OUTPUT_SEGMENT_NAME};

    let node = fresh_node().await;
    node.add_schema(AGENT_OUTPUT_SEGMENT).await.unwrap();
    let before = node
        .get_collection(AGENT_OUTPUT_SEGMENT_NAME)
        .unwrap()
        .unwrap();
    let mut inputs = (0..32)
        .map(|turn| serde_json::json!({
            "request_doc_id": "request-a",
            "source": {"kind": "provider_turn", "scope": "inference.1", "turn_index": turn, "attempt": 0},
            "payload": format!("turn-{turn}"),
        }))
        .collect::<Vec<_>>();
    for (request, payload) in [
        ("request-a", "conflict-1"),
        ("request-a", "conflict-2"),
        ("request-b", "foreign"),
    ] {
        inputs.push(serde_json::json!({
            "request_doc_id": request,
            "source": {"kind": "provider_turn", "scope": "inference.1", "turn_index": 17, "attempt": 0},
            "payload": payload,
        }));
    }
    let input = gents_protocol::graphql::graphql_input_literal(&Value::Array(inputs)).unwrap();
    let created = node
        .execute(&format!(
            "mutation {{ create_AgentOutputSegment(input: {input}) {{ _docID }} }}"
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);

    let baseline = DEFAULT_BASELINE
        .iter()
        .find(|entry| entry.name == AGENT_OUTPUT_SEGMENT_NAME)
        .unwrap();
    let index = DEFAULT_ORDINARY_INDEXES
        .iter()
        .find(|index| index.collection == AGENT_OUTPUT_SEGMENT_NAME)
        .unwrap();
    let registry = Registry {
        baseline: std::slice::from_ref(baseline),
        steps: &[],
        ordinary_indexes: std::slice::from_ref(index),
    };
    for _ in 0..2 {
        ensure_migrations_with_registry(node.as_ref(), &registry)
            .await
            .unwrap();
        let after = node
            .get_collection(AGENT_OUTPUT_SEGMENT_NAME)
            .unwrap()
            .unwrap();
        assert_eq!(before.version_id, after.version_id);
        assert_eq!(before.collection_id, after.collection_id);
        let query = r#"query { AgentOutputSegment(filter: {
            request_doc_id: {_eq: "request-a"}, source: {turn_index: {_eq: 17}}
        }) { payload } }"#;
        let rows = node.execute(query).await;
        assert!(!rows.has_errors(), "{:?}", rows.errors);
        let mut payloads = rows.data.unwrap()["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["payload"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        payloads.sort();
        assert_eq!(payloads, ["conflict-1", "conflict-2", "turn-17"]);
        assert_indexed_query(node.as_ref(), query, index.name, 3).await;
    }
    node.shutdown().await;
}

#[tokio::test]
async fn background_completion_recovery_seeks_pending_status_range() {
    use gents_protocol::schemas::{AGENT_TOOL_CALL, AGENT_TOOL_CALL_NAME};
    let node = fresh_node().await;
    node.add_schema(AGENT_TOOL_CALL).await.unwrap();
    let before = node.get_collection(AGENT_TOOL_CALL_NAME).unwrap().unwrap();
    let historical = (0..160)
        .map(|i| {
            serde_json::json!({
                "tool_call_key": format!("old-{i}"), "node_did":"n", "await_mode":"background",
                "lifecycle_state":"completed", "status":"completed"
            })
        })
        .collect::<Vec<_>>();
    let input = gents_protocol::graphql::graphql_input_literal(&Value::Array(historical)).unwrap();
    let created = node
        .execute(&format!(
            "mutation {{ create_AgentToolCall(input:{input}) {{_docID}} }}"
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let baseline = DEFAULT_BASELINE
        .iter()
        .find(|c| c.name == AGENT_TOOL_CALL_NAME)
        .unwrap();
    let index = DEFAULT_ORDINARY_INDEXES
        .iter()
        .find(|i| i.name == "AgentToolCall_node_await_status")
        .unwrap();
    let registry = Registry {
        baseline: std::slice::from_ref(baseline),
        steps: &[],
        ordinary_indexes: std::slice::from_ref(index),
    };
    ensure_migrations_with_registry(node.as_ref(), &registry)
        .await
        .unwrap();
    let query = r#"query { AgentToolCall(filter:{node_did:{_eq:"n"},await_mode:{_eq:"background"},lifecycle_state:{_in:["completed","failed","timedOut","cancelled"]},status:{_like:"completionPending%",_ge:"completionPending",_lt:"completionPendinh"}}){tool_call_key} }"#;
    assert_indexed_query(node.as_ref(), query, index.name, 0).await;
    let cases = serde_json::json!([
        {"tool_call_key":"pending","node_did":"n","await_mode":"background","lifecycle_state":"completed","status":"completionPending:success"},
        {"tool_call_key":"bare-prefix","node_did":"n","await_mode":"background","lifecycle_state":"failed","status":"completionPending"},
        {"tool_call_key":"unicode","node_did":"n","await_mode":"background","lifecycle_state":"cancelled","status":"completionPending:é"},
        {"tool_call_key":"running","node_did":"n","await_mode":"background","lifecycle_state":"running","status":"completionPending:success"},
        {"tool_call_key":"foreground","node_did":"n","await_mode":"foreground","lifecycle_state":"completed","status":"completionPending:success"},
        {"tool_call_key":"foreign","node_did":"other","await_mode":"background","lifecycle_state":"completed","status":"completionPending:success"},
        {"tool_call_key":"past-prefix","node_did":"n","await_mode":"background","lifecycle_state":"completed","status":"completionPendinh"}
    ]);
    let input = gents_protocol::graphql::graphql_input_literal(&cases).unwrap();
    let created = node
        .execute(&format!(
            "mutation {{ create_AgentToolCall(input:{input}) {{_docID}} }}"
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let legacy = query.replace(",_ge:\"completionPending\",_lt:\"completionPendinh\"", "");
    let mut observed = Vec::new();
    for q in [query, &legacy] {
        let result = node.execute(q).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        let mut keys = result.data.unwrap()["AgentToolCall"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["tool_call_key"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        keys.sort();
        observed.push(keys);
    }
    assert_eq!(observed[0], ["bare-prefix", "pending", "unicode"]);
    assert_eq!(observed[0], observed[1]);
    assert_indexed_query(node.as_ref(), query, index.name, 4).await;
    assert_eq!(
        before.version_id,
        node.get_collection(AGENT_TOOL_CALL_NAME)
            .unwrap()
            .unwrap()
            .version_id
    );
    node.shutdown().await;
}

#[tokio::test]
async fn session_only_message_page_uses_ordered_index_with_tiebreaker() {
    use gents_protocol::schemas::{AGENT_MESSAGE, AGENT_MESSAGE_NAME};
    let node = fresh_node().await;
    node.add_schema(AGENT_MESSAGE).await.unwrap();
    let before = node.get_collection(AGENT_MESSAGE_NAME).unwrap().unwrap();
    let input=Value::Array((0..160).map(|i|serde_json::json!({"message_key":format!("message-{i}"),"node_did":"n","session_id":"s","sequence":i})).collect());
    let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
    let created = node
        .execute(&format!(
            "mutation {{create_AgentMessage(input:{input}){{_docID}}}}"
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let query = r#"query {AgentMessage(filter:{session_id:{_eq:"s"}},order:[{sequence:DESC},{_docID:ASC}],limit:30){_docID sequence}}"#;
    let original = node.execute(query).await;
    assert!(!original.has_errors(), "{:?}", original.errors);
    let baseline = DEFAULT_BASELINE
        .iter()
        .find(|c| c.name == AGENT_MESSAGE_NAME)
        .unwrap();
    let index = DEFAULT_ORDINARY_INDEXES
        .iter()
        .find(|i| i.name == "AgentMessage_session_sequence")
        .unwrap();
    let registry = Registry {
        baseline: std::slice::from_ref(baseline),
        steps: &[],
        ordinary_indexes: std::slice::from_ref(index),
    };
    ensure_migrations_with_registry(node.as_ref(), &registry)
        .await
        .unwrap();
    let result = node.execute(query).await;
    assert!(!result.has_errors(), "{:?}", result.errors);
    assert_eq!(original.data, result.data);
    assert_indexed_query(node.as_ref(), query, index.name, 31).await;
    assert_eq!(
        before.version_id,
        node.get_collection(AGENT_MESSAGE_NAME)
            .unwrap()
            .unwrap()
            .version_id
    );
    node.shutdown().await;
}

#[tokio::test]
async fn transcript_tool_page_streams_sequence_and_key_order() {
    use gents_protocol::schemas::{AGENT_TOOL_CALL, AGENT_TOOL_CALL_NAME};
    let node = fresh_node().await;
    node.add_schema(AGENT_TOOL_CALL).await.unwrap();
    let before = node.get_collection(AGENT_TOOL_CALL_NAME).unwrap().unwrap();
    let rows = (0..160).map(|i| serde_json::json!({
        "tool_call_key": format!("tool-{i:03}"),
        "session_id": if i < 80 { "s" } else { "other-session" },
        "node_did": "n",
        "requester_did": "r",
        "message_sequence": i / 2,
    })).chain([
        serde_json::json!({"tool_call_key":"foreign-node", "session_id":"s", "node_did":"other", "requester_did":"r", "message_sequence":100}),
        serde_json::json!({"tool_call_key":"foreign-requester", "session_id":"s", "node_did":"n", "requester_did":"other", "message_sequence":100}),
    ]).collect::<Vec<_>>();
    let input = gents_protocol::graphql::graphql_input_literal(&Value::Array(rows)).unwrap();
    let created = node
        .execute(&format!(
            "mutation {{ create_AgentToolCall(input:{input}) {{_docID}} }}"
        ))
        .await;
    assert!(!created.has_errors(), "{:?}", created.errors);
    let query = r#"query { AgentToolCall(filter:{session_id:{_eq:"s"},node_did:{_eq:"n"},requester_did:{_eq:"r"}},order:[{message_sequence:DESC},{tool_call_key:DESC}],limit:10){_docID message_sequence tool_call_key} }"#;
    let original = node.execute(query).await;
    assert!(!original.has_errors(), "{:?}", original.errors);
    let baseline = DEFAULT_BASELINE
        .iter()
        .find(|c| c.name == AGENT_TOOL_CALL_NAME)
        .unwrap();
    let index = DEFAULT_ORDINARY_INDEXES
        .iter()
        .find(|i| i.name == "AgentToolCall_session_sequence_key")
        .unwrap();
    let registry = Registry {
        baseline: std::slice::from_ref(baseline),
        steps: &[],
        ordinary_indexes: std::slice::from_ref(index),
    };
    for _ in 0..2 {
        ensure_migrations_with_registry(node.as_ref(), &registry)
            .await
            .unwrap();
        let result = node.execute(query).await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        assert_eq!(original.data, result.data);
        let keys = result.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["tool_call_key"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            (70..80)
                .rev()
                .map(|i| format!("tool-{i:03}"))
                .collect::<Vec<_>>()
        );
        assert_indexed_query(node.as_ref(), query, index.name, 12).await;
        assert_eq!(
            before.version_id,
            node.get_collection(AGENT_TOOL_CALL_NAME)
                .unwrap()
                .unwrap()
                .version_id
        );
    }
    node.shutdown().await;
}
