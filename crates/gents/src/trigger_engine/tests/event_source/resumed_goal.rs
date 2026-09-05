use super::*;
use crate::defra_write::{BoundedWriteParams, BoundedWriteTool};
use crate::document_config::{WriteToolDecl, WriteToolField, WriteToolFieldFill};
use crate::identity::AgentIdentity;
use crate::llm::tool::Tool;
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::row::AgentRequestRow;

#[tokio::test]
async fn resumed_goal_tool_output_reaches_correlation_keyed_event_trigger() {
    let temp = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(temp.path().join("principal.key"), None).unwrap();
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    ensure_runtime_schemas(&node).await.unwrap();
    node.add_schema("type ResumedGoalOutput { run_id: String @index summary: String }")
        .await
        .unwrap();
    crate::goal::set_goal(
        &node,
        identity.did(),
        "resumed-event-session",
        Some("Emit the remaining correlated result"),
        Some(crate::goal::GoalStatus::Paused),
        Some(Some(1000)),
    )
    .await
    .unwrap();
    let mut parent = AgentRequestCreate::base(
        "resumed-event-parent",
        identity.did(),
        identity.did(),
        "general",
        "resumed-event-session",
        "Original correlated work",
        "interactive",
        "2020-01-01T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    parent.caused_by_correlation = Some("resumed-run-correlation".into());
    crate::sign_agent_request_create(&identity, &mut parent)
        .await
        .unwrap();
    let response = node.execute(&parent.graphql_mutation().unwrap()).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let response = node
        .execute(
            r#"mutation {
        update_AgentRequest(filter: {request_id: {_eq: "resumed-event-parent"}},
            input: {lifecycle_state: "completed"}) { _docID }
    }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let receipt = crate::goal::resume_goal_request(
        &crate::ConfigAccess::Local(node.clone()),
        &identity,
        identity.did(),
        "resumed-event-session",
        "resumed-event-parent",
    )
    .await
    .unwrap();
    assert!(receipt.created);
    let response = node
        .execute(&format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{ {} }} }}",
            escape_graphql_string(&receipt.request_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response.data.unwrap()["AgentRequest"].clone()).unwrap();
    assert_eq!(rows.len(), 1);
    let child = crate::watcher::AgentRequest::try_from(rows[0].clone()).unwrap();
    assert_eq!(
        child.caused_by_correlation.as_deref(),
        Some("resumed-run-correlation")
    );

    let trigger = ResolvedEventTrigger {
        correlation_field: Some("run_id".into()),
        ..resolved_event_trigger_with_filter(
            "resumed-output-trigger",
            "ResumedGoalOutput",
            resolved_task("{{ event.correlation }}"),
            r#"{ run_id: { _eq: "resumed-run-correlation" } }"#,
        )
    };
    let snapshot = snapshot_with_event_triggers(
        1,
        HashMap::from([("resumed-output-trigger".to_owned(), trigger)]),
    );
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let tool = BoundedWriteTool::new(
        node.clone(),
        WriteToolDecl {
            tool_name: "write_resumed_output".into(),
            collection: "ResumedGoalOutput".into(),
            description: "Publish the correlated result".into(),
            fields: vec![
                WriteToolField {
                    name: "summary".into(),
                    required: true,
                    fill: None,
                },
                WriteToolField {
                    name: "run_id".into(),
                    required: false,
                    fill: Some(WriteToolFieldFill::Correlation),
                },
            ],
            output_obligation: None,
        },
    );
    // Use the persisted resumed request at the daemon's actual tool scope seam.
    // The tool receives no model-authored run_id and must stamp it itself.
    crate::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None,
        CancellationToken::new(),
        None,
        None,
        Some(child.session_id.clone()),
        child.caused_by_correlation.clone(),
        Default::default(),
        false,
        async {
            Tool::call(
                &tool,
                BoundedWriteParams(
                    serde_json::json!({"summary": "remaining work complete"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
        },
    )
    .await;
    let response = node
        .execute("{ ResumedGoalOutput { _docID run_id summary } }")
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let outputs = response.data.unwrap()["ResumedGoalOutput"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0]["run_id"], "resumed-run-correlation");
    let intent = tokio::time::timeout(Duration::from_secs(2), source.next_fire())
        .await
        .expect("resumed correlated output delivery timed out")
        .expect("resumed correlated output should pass the event trigger filter");
    assert_eq!(
        intent.correlation.as_deref(),
        Some("resumed-run-correlation")
    );
    assert_eq!(intent.event_vars["source_doc_id"], outputs[0]["_docID"]);
}
