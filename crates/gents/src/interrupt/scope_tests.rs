use super::*;
use serde_json::{json, Value};

#[tokio::test]
async fn generated_interrupt_drain_preserves_foreign_owner_and_requester_queues() {
    let cases = crate::lean_vocab_test::lean_queue_deadline_cases();
    let case = cases
        .iter()
        .find(|case| case.name == "cancel_drains_automated_wakeups_preserves_user_pending")
        .unwrap();
    assert_eq!(case.preserved_foreign_requester_request_ids.len(), 1);
    assert_eq!(case.preserved_foreign_owner_request_ids.len(), 1);
    let dir = tempfile::tempdir().unwrap();
    let node = EmbeddedNode::builder()
        .data_path(dir.path().join("db"))
        .build()
        .await
        .unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let session = case.session_id.to_string();
    let owner = "did:test:drain-owner";
    let foreign_owner = "did:test:foreign-owner";
    let requester = "did:test:foreign-requester";
    async fn create(
        node: &EmbeddedNode,
        id: &str,
        owner: &str,
        requester: Option<&str>,
        session: &str,
        source: Option<&str>,
    ) -> String {
        let input=source.map(|source|json!({"queue":{"source":source,"policy":if source=="background_completion" {"coalesce"} else {"append"},"key":if source=="background_completion" {Some(format!("background_completion:{session}"))}else{None}}}));
        let value = json!({"request_id":id,"agent_did":owner,"requester_did":requester,"session_id":session,
            "behavior_id":"configured","lifecycle_state":"pending","execution_origin":if source.is_some(){"scheduled"}else{"interactive"},"input":input,"created_at":"2026-09-01T00:00:00Z"});
        crate::config_client::ConfigAccess::transact_local(
            node, None, "test.interrupt.scope_fixture", |txn| { let value = value.clone(); Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": value}),
                ).await?;
                gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentRequest")
            }) },
        ).await.unwrap()
    }
    async fn row(node: &EmbeddedNode, doc: &str) -> Value {
        let doc = escape_graphql_string(doc);
        let response=node.execute(&format!("{{AgentRequest(filter:{{_docID:{{_eq:\"{doc}\"}}}}){{lifecycle_state interrupt_requested_at}}}}")).await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()["AgentRequest"][0].clone()
    }
    let parent = create(&node, "same-parent-label", owner, None, &session, None).await;
    let foreign_parent = create(
        &node,
        "same-parent-label",
        foreign_owner,
        None,
        &session,
        None,
    )
    .await;
    let own = create(
        &node,
        &case.automated_drained_request_ids[0].to_string(),
        owner,
        None,
        &session,
        Some("background_completion"),
    )
    .await;
    let user = create(
        &node,
        &case.preserved_user_pending_request_ids[0].to_string(),
        owner,
        None,
        &session,
        Some("user"),
    )
    .await;
    let foreign_requester = create(
        &node,
        &case.preserved_foreign_requester_request_ids[0].to_string(),
        owner,
        Some(requester),
        &session,
        Some("background_completion"),
    )
    .await;
    let foreign = create(
        &node,
        &case.preserved_foreign_owner_request_ids[0].to_string(),
        foreign_owner,
        None,
        &session,
        Some("background_completion"),
    )
    .await;
    assert!(
        interrupt_request(&node, "same-parent-label").await.is_err(),
        "global label collision must not choose one principal"
    );
    assert!(
        interrupt_request_by_doc_id(&node, &parent, foreign_owner, None)
            .await
            .is_err()
    );
    assert!(
        interrupt_request_by_doc_id(&node, &parent, owner, Some(requester))
            .await
            .is_err()
    );
    assert!(row(&node, &parent).await["interrupt_requested_at"].is_null());
    interrupt_request_by_doc_id(&node, &parent, owner, None)
        .await
        .unwrap();
    assert_eq!(row(&node, &own).await["lifecycle_state"], "interrupted");
    for doc in [&user, &foreign_requester, &foreign, &foreign_parent] {
        let row = row(&node, doc).await;
        assert_eq!(row["lifecycle_state"], "pending");
        assert!(row["interrupt_requested_at"].is_null());
    }
    let first = row(&node, &parent).await["interrupt_requested_at"].clone();
    interrupt_request_by_doc_id(&node, &parent, owner, None)
        .await
        .unwrap();
    assert_eq!(
        row(&node, &parent).await["interrupt_requested_at"],
        first,
        "repeat interrupt preserves latch timestamp"
    );
    node.shutdown().await;
}
