mod support;

use chrono::{Duration, Utc};
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{
    create_subagent_request_with_request_id,
    create_subagent_request_with_trusted_parent_request_id, AwaitMode, CancelPolicy,
    ToolCallLifecycle,
};
use gents::{
    resolve_descendant_edge, resolve_descendant_graph, DescendantControlAuthority,
    DescendantGraphAccess, DescendantMaterializationState, DescendantQuery, DescendantScope,
};

async fn create_request(
    node: &gents::defra_node::EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    behavior_id: &str,
) -> String {
    let request_id = escape_graphql_string(request_id);
    let session_id = escape_graphql_string(session_id);
    let agent_did = escape_graphql_string(agent_did);
    let behavior_id = escape_graphql_string(behavior_id);
    let now = Utc::now().to_rfc3339();
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_AgentRequest(input: {{
                    request_id: "{request_id}",
                    agent_did: "{agent_did}",
                    behavior_id: "{behavior_id}",
                    session_id: "{session_id}",
                    retry_parent_request: "",
                    retry_root_request: "{request_id}",
                    superseded_by_request: "",
                    content: "descendant graph root",
                    status: "processing",
                    lifecycle_state: "processing",
                    backend_id: "",
                    execution_origin: "interactive",
                    metadata: "",
                    failure_reason: "",
                    created_at: "{now}",
                    retry_count: 0,
                    max_retries: 3,
                    subagent_depth: 0
                }}) {{ _docID }}
            }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    support::exact_request_doc_id(node, &request_id).await
}

async fn create_nested_bridge(
    node: &gents::defra_node::EmbeddedNode,
    parent_request_doc_id: &str,
    parent_session_id: &str,
    requester_did: &str,
) -> String {
    let args = escape_graphql_string(
        r#"{"name":"reviewer","behavior_id":"reviewer","workflow_task_id":"review"}"#,
    );
    let now = Utc::now().to_rfc3339();
    let deadline = (Utc::now() + Duration::minutes(5)).to_rfc3339();
    let response = node
        .execute(&format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{parent_session_id}:call-reviewer",
                    request_id: "request-worker",
                    request_doc_id: "{parent_request_doc_id}",
                    session_id: "{parent_session_id}",
                    agent_did: "did:test:worker-host",
                    requester_did: "{requester_did}",
                    message_sequence: 1,
                    tool_name: "spawn_subagent",
                    tool_call_id: "call-reviewer",
                    args: "{args}",
                    result: "",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "{now}",
                    deadline_at: "{deadline}",
                    child_request_id: "request-reviewer",
                    spawn_target_did: "did:test:worker-host",
                    await_mode: "foreground",
                    cancel_policy: "cascade",
                    workflow_group_id: "workflow-1",
                    workflow_role: "reviewer",
                    selected_service_id: null,
                    selected_tool_name: null,
                    tool_failure_class: null,
                    latency_ms: null
                }}) {{ _docID }}
            }}"#,
            parent_request_doc_id = escape_graphql_string(parent_request_doc_id),
            parent_session_id = escape_graphql_string(parent_session_id),
            requester_did = escape_graphql_string(requester_did),
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let response = node
        .execute(
            r#"{ AgentToolCall(filter: { tool_call_id: { _eq: "call-reviewer" } }, limit: 1) { _docID } }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    response.data.expect("bridge query data")["AgentToolCall"][0]["_docID"]
        .as_str()
        .expect("reviewer bridge doc")
        .to_string()
}

#[tokio::test]
async fn canonical_graph_preserves_pending_remote_terminal_nested_and_paging_edges() {
    let db = support::test_db("canonical-descendant-graph").await;
    let root_id = "graph-root";
    let root_session = "graph-root-session";
    let root_did = "did:test:coordinator";
    let root_doc =
        create_request(db.node.as_ref(), root_id, root_session, root_did, "default").await;

    let deadline = Utc::now() + Duration::minutes(5);
    let mut worker_bridge = ToolCallLifecycle::new_subagent(
        db.node.clone(),
        root_id.to_string(),
        root_session.to_string(),
        root_did.to_string(),
        "call-worker".to_string(),
        1,
        "spawn_subagent".to_string(),
        r#"{"name":"fast-worker","behavior_id":"fast-worker"}"#.to_string(),
        deadline,
        AwaitMode::Background,
        CancelPolicy::Cascade,
        "request-worker".to_string(),
        "did:test:worker-host".to_string(),
    )
    .with_request_doc_id(Some(root_doc.clone()));
    worker_bridge.start_running().await.unwrap();
    let worker_bridge_doc = worker_bridge.doc_id().unwrap().to_string();

    let pending = resolve_descendant_graph(
        DescendantGraphAccess::Local(db.node.as_ref()),
        &DescendantQuery::direct(root_id),
    )
    .await
    .unwrap();
    assert_eq!(pending.edges.len(), 1);
    assert_eq!(
        pending.edges[0].materialization_state,
        DescendantMaterializationState::AwaitingChild
    );
    assert_eq!(pending.edges[0].child_request_id, "request-worker");
    let pending_exact = resolve_descendant_edge(
        DescendantGraphAccess::Local(db.node.as_ref()),
        root_id,
        "request-worker",
    )
    .await
    .unwrap()
    .expect("pending exact edge");
    assert_eq!(pending_exact.depth, 1);
    assert!(!pending_exact.readable());
    assert!(pending_exact.retryable());
    let pending_cursor = pending.edges[0].cursor.clone();

    create_subagent_request_with_trusted_parent_request_id(
        db.node.as_ref(),
        "request-worker".to_string(),
        root_id.to_string(),
        root_doc.clone(),
        "call-worker".to_string(),
        worker_bridge_doc.clone(),
        0,
        "did:test:worker-host".to_string(),
        "fast-worker".to_string(),
        "work".to_string(),
        Some(deadline),
        root_did.to_string(),
    )
    .await
    .unwrap();
    let worker_doc = support::exact_request_doc_id(db.node.as_ref(), "request-worker").await;

    let materialized = resolve_descendant_graph(
        DescendantGraphAccess::Local(db.node.as_ref()),
        &DescendantQuery::direct(root_id),
    )
    .await
    .unwrap();
    assert_eq!(materialized.edges[0].cursor, pending_cursor);
    assert!(materialized.edges[0].readable());
    assert_eq!(
        materialized.edges[0].behavior_id.as_deref(),
        Some("fast-worker")
    );
    assert_eq!(
        materialized.edges[0].materialization_state,
        DescendantMaterializationState::MaterializedRemote
    );

    let worker_session = materialized.edges[0]
        .child_session_id
        .clone()
        .expect("worker session");
    let reviewer_bridge_doc =
        create_nested_bridge(db.node.as_ref(), &worker_doc, &worker_session, root_did).await;
    create_subagent_request_with_request_id(
        db.node.as_ref(),
        "request-reviewer".to_string(),
        "request-worker".to_string(),
        worker_doc,
        "call-reviewer".to_string(),
        reviewer_bridge_doc,
        1,
        "did:test:worker-host".to_string(),
        "reviewer".to_string(),
        "review".to_string(),
        Some(deadline),
    )
    .await
    .unwrap();

    worker_bridge
        .bridge_complete("durable worker result".to_string())
        .await
        .unwrap();

    let all = resolve_descendant_graph(
        DescendantGraphAccess::Local(db.node.as_ref()),
        &DescendantQuery::all(root_id),
    )
    .await
    .unwrap();
    assert_eq!(all.edges.len(), 2);
    assert!(all.edges[0].terminal_result_ref.is_some());
    assert_eq!(all.edges[1].depth, 2);
    assert_eq!(
        all.edges[1].control_authority,
        DescendantControlAuthority::VisibilityOnly
    );
    assert_eq!(all.edges[1].workflow_role.as_deref(), Some("reviewer"));

    let direct_exact = resolve_descendant_edge(
        DescendantGraphAccess::Local(db.node.as_ref()),
        root_id,
        "request-worker",
    )
    .await
    .unwrap()
    .expect("direct exact edge");
    assert_eq!(direct_exact.depth, 1);
    assert!(direct_exact.readable());
    let nested_exact = resolve_descendant_edge(
        DescendantGraphAccess::Local(db.node.as_ref()),
        root_id,
        "request-reviewer",
    )
    .await
    .unwrap()
    .expect("nested exact edge");
    assert_eq!(nested_exact.depth, 2);
    assert!(nested_exact.readable());
    assert_eq!(
        nested_exact.control_authority,
        DescendantControlAuthority::VisibilityOnly
    );
    assert!(resolve_descendant_edge(
        DescendantGraphAccess::Local(db.node.as_ref()),
        root_id,
        "unrelated-child",
    )
    .await
    .unwrap()
    .is_none());
    assert!(
        resolve_descendant_edge(
            DescendantGraphAccess::Local(db.node.as_ref()),
            "request-reviewer",
            "request-worker",
        )
        .await
        .unwrap()
        .is_none(),
        "a descendant root cannot enumerate an ancestor edge"
    );

    let workflow = resolve_descendant_graph(
        DescendantGraphAccess::Local(db.node.as_ref()),
        &DescendantQuery {
            root_request_id: root_id.to_string(),
            scope: DescendantScope::WorkflowGroup,
            workflow_group_id: Some("workflow-1".to_string()),
            after: None,
            limit: 20,
            include_terminal: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(workflow.edges.len(), 1);
    assert_eq!(workflow.edges[0].child_request_id, "request-reviewer");

    let first = resolve_descendant_graph(
        DescendantGraphAccess::Local(db.node.as_ref()),
        &DescendantQuery {
            limit: 1,
            ..DescendantQuery::all(root_id)
        },
    )
    .await
    .unwrap();
    assert!(first.has_more);
    let second = resolve_descendant_graph(
        DescendantGraphAccess::Local(db.node.as_ref()),
        &DescendantQuery {
            after: first.next_cursor,
            limit: 1,
            ..DescendantQuery::all(root_id)
        },
    )
    .await
    .unwrap();
    assert_eq!(second.edges[0].child_request_id, "request-reviewer");
}
