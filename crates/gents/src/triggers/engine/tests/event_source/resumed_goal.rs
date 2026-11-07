use super::*;
use crate::documents::{WriteToolDecl, WriteToolField, WriteToolFieldFill};
use crate::inference::llm::tool::Tool;
use crate::store::identity::NodeIdentity;
use crate::tools::builtin::defra_write::{BoundedWriteParams, BoundedWriteTool};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
use gents_protocol::row::AgentRequestRow;

#[tokio::test]
async fn resumed_goal_tool_output_reaches_correlation_keyed_event_trigger() {
    let node = configured_event_node().await;
    node.add_collection("type ResumedGoalOutput { run_id: String @index summary: String }")
        .await
        .unwrap();
    let identity = crate::triggers::callback::test_root_identity();
    let agent = crate::triggers::callback::test_agent_identity();
    let access = crate::ConfigAccess::local_managed_agent(
        node.clone(),
        ::identity::Did::new(agent.did()).unwrap(),
        agent.as_ref(),
    )
    .unwrap();
    access
        .transact_write("test.resumed_goal_session", |txn| {
            let (identity, agent) = (&identity, &agent);
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    "resumed-event-session",
                    identity.did(),
                    agent.did(),
                    Some(agent.did()),
                    None,
                    None,
                    "2020-01-01T00:00:00Z",
                )
                .await?;
                // A created session admits requests only once its creator has
                // published the session's Agent relationships.
                let session = crate::session::load_agent_session_owner_in_txn(
                    txn,
                    identity.did(),
                    "resumed-event-session",
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("fixture session is unavailable"))?;
                let (agent_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    crate::Collection::Agent,
                    identity.did(),
                    agent.did(),
                )
                .await?
                .ok_or_else(|| anyhow::anyhow!("fixture Agent is unavailable"))?;
                crate::session::stage_created_session_relationships_in_txn(
                    txn,
                    &session.doc_id,
                    &agent_doc,
                    &::identity::Did::new(agent.did())?,
                )
                .await
            })
        })
        .await
        .unwrap();
    let goal = crate::goal::set_goal_from_access(
        &access,
        identity.did(),
        "resumed-event-session",
        Some("Emit the remaining correlated result"),
        Some(crate::goal::GoalStatus::Paused),
        Some(Some(1000)),
        None,
    )
    .await
    .unwrap();
    let mut parent = AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        "resumed-event-parent",
        identity.did(),
        agent.did(),
        agent.did(),
        "resumed-event-session",
        "Original correlated work",
        "interactive",
        "2020-01-01T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(agent.did()),
    );
    parent.caused_by_correlation = Some("resumed-run-correlation".into());
    crate::sign_agent_request_create(agent.as_ref(), &mut parent)
        .await
        .unwrap();
    let response = access
        .write(
            "test.resumed_goal_parent",
            &parent.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let parent_doc = crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    access.transact_write("test.bind_resumed_goal", |txn| {
        let goal = &goal;
        let parent_doc = &parent_doc;
        Box::pin(async move {
            crate::goal::bind_goal_first_request_in_txn(txn, goal, parent_doc, None).await?;
            txn.execute(&format!(r#"mutation {{ update_AgentRequest(docID: "{}", input: {{lifecycle_state: "completed"}}) {{_docID}} }}"#, escape_graphql_string(parent_doc))).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    let receipt = crate::goal::resume_goal_request(
        &access,
        agent.as_ref(),
        identity.did(),
        "resumed-event-session",
        "resumed-event-parent",
    )
    .await
    .unwrap();
    assert!(receipt.created);
    let response = access
        .execute(&format!(
            "{{ AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{ {} }} }}",
            escape_graphql_string(&receipt.request_id),
            crate::request::admission::SIGNED_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    assert_eq!(rows.len(), 1);
    let child = crate::request::watcher::AgentRequest::try_from(rows[0].clone()).unwrap();
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
    let snapshot = configured_event_snapshot(
        &node,
        1,
        RapidHashMap::from_iter([("resumed-output-trigger".to_owned(), trigger)]),
    )
    .await;
    let (_tx, rx) = watch::channel(snapshot.clone());
    let mut source = EventSource::new(rx, node.clone(), CancellationToken::new());
    source.reconcile_subscriptions(snapshot.as_ref()).await;
    let tool = BoundedWriteTool::new(
        crate::store::config_client::ConfigAccess::local(node.clone()),
        WriteToolDecl {
            notification: None,
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
    crate::tools::call::lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
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
