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
    let node = Arc::new(
        crate::triggers::callback::test_node_builder()
            .data_path(dir.path().join("db"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let root_did = crate::request::lifecycle::test_support::PIN_FIXED_DID;
    crate::triggers::callback::seed_test_agent(&node, root_did).await;
    let agent = crate::triggers::callback::test_agent_identity();
    let signer = agent.managed_agent_signer().unwrap();
    let signer = Some(&signer);
    let session = case.session_id.to_string();
    use crate::store::identity::NodeIdentity as _;
    let root = crate::triggers::callback::test_root_identity();
    let derived_did = |nonce: char| {
        crate::store::identity::derive_agent_identity(&root, &nonce.to_string().repeat(64))
            .unwrap()
            .did()
            .to_owned()
    };
    let owner_did = root.did().to_owned();
    let foreign_owner_did = derived_did('d');
    let requester_did = derived_did('e');
    let owner = owner_did.as_str();
    let foreign_owner = foreign_owner_did.as_str();
    let requester = requester_did.as_str();
    /// The runtime root creates each row for the fixture Agent and grants it
    /// as working Agent; only the node and requester scopes differ.
    async fn create(
        node: &Arc<EmbeddedNode>,
        id: &str,
        owner: &str,
        requester: Option<&str>,
        session: &str,
        source: Option<&str>,
    ) -> String {
        let agent = crate::triggers::callback::test_agent_did();
        let input=source.map(|source|json!({"queue":{"source":source,"policy":if source=="background_completion" {"coalesce"} else {"append"},"key":if source=="background_completion" {Some(format!("background_completion:{session}"))}else{None}}}));
        let value = json!({"request_id":id,"purpose":"normal","node_did":owner,"requester_did":requester,"session_id":session,
            "agent_did":agent,"lifecycle_state":"pending","execution_origin":if source.is_some(){"scheduled"}else{"interactive"},"input":input,"created_at":"2026-09-01T00:00:00Z"});
        let root =
            ::identity::Did::new(crate::request::lifecycle::test_support::PIN_FIXED_DID).unwrap();
        crate::store::config_client::ConfigAccess::transact_local_write(
            node, Some(root), "test.interrupt.scope_fixture", |txn| { let (value, agent) = (value.clone(), agent.clone()); Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": value}),
                ).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentRequest")?;
                txn.add_actor_relationship("AgentRequest", &doc, "working_agent", &::identity::Did::new(&agent)?).await?;
                Ok::<_, anyhow::Error>(doc)
            }) },
        ).await.unwrap()
    }
    async fn row(node: &Arc<EmbeddedNode>, doc: &str) -> Value {
        let doc = escape_graphql_string(doc);
        let response = crate::triggers::callback::test_agent_access(node)
            .execute(&format!("{{AgentRequest(filter:{{_docID:{{_eq:\"{doc}\"}}}}){{lifecycle_state interrupt_requested_at}}}}"))
            .await
            .unwrap();
        response["data"]["AgentRequest"][0].clone()
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
        interrupt_request(&node, agent.as_ref(), "same-parent-label")
            .await
            .is_err()
    );
    assert!(
        interrupt_request_by_doc_id(&node, signer, &parent, foreign_owner, None)
            .await
            .is_err()
    );
    assert!(
        interrupt_request_by_doc_id(&node, signer, &parent, owner, Some(requester))
            .await
            .is_err()
    );
    // Without the working Agent's managed signer the latch is refused rather
    // than attempted as another actor.
    let unsigned = interrupt_request_by_doc_id(&node, None, &parent, owner, None).await;
    assert!(matches!(
        unsigned
            .unwrap_err()
            .downcast_ref::<crate::store::config_client::ConfigClientError>(),
        Some(crate::store::config_client::ConfigClientError::ManagedAgentSignerRequired)
    ));
    assert!(row(&node, &parent).await["interrupt_requested_at"].is_null());
    interrupt_request_by_doc_id(&node, signer, &parent, owner, None)
        .await
        .unwrap();
    assert_eq!(row(&node, &own).await["lifecycle_state"], "interrupted");
    for doc in [&user, &foreign_requester, &foreign, &foreign_parent] {
        let row = row(&node, doc).await;
        assert_eq!(row["lifecycle_state"], "pending");
        assert!(row["interrupt_requested_at"].is_null());
    }
    let first = row(&node, &parent).await["interrupt_requested_at"].clone();
    interrupt_request_by_doc_id(&node, signer, &parent, owner, None)
        .await
        .unwrap();
    assert_eq!(
        row(&node, &parent).await["interrupt_requested_at"],
        first,
        "repeat interrupt preserves latch timestamp"
    );
    node.shutdown().await.expect("shut down test node");
}

#[tokio::test]
async fn logical_interrupt_latches_only_as_the_signed_working_agent() {
    let dir = tempfile::tempdir().unwrap();
    let node = Arc::new(
        crate::triggers::callback::test_node_builder()
            .data_path(dir.path().join("db"))
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let root_did = crate::request::lifecycle::test_support::PIN_FIXED_DID;
    crate::triggers::callback::seed_test_agent(&node, root_did).await;
    let agent = crate::triggers::callback::test_agent_identity();
    let value = json!({"request_id":"logical-latch","purpose":"normal","node_did":root_did,
        "session_id":"logical-latch-session","agent_did":agent.did(),"lifecycle_state":"pending",
        "execution_origin":"interactive","created_at":"2026-09-01T00:00:00Z"});
    let agent_did = agent.did().to_owned();
    let doc = crate::store::config_client::ConfigAccess::transact_local_write(
        &node,
        Some(::identity::Did::new(root_did).unwrap()),
        "test.interrupt.logical_fixture",
        |txn| {
            let (value, agent_did) = (value.clone(), agent_did.clone());
            Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({"input": value}),
                ).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentRequest")?;
                txn.add_actor_relationship("AgentRequest", &doc, "working_agent", &::identity::Did::new(&agent_did)?).await?;
                Ok::<_, anyhow::Error>(doc)
            })
        },
    )
    .await
    .unwrap();
    let latched = || async {
        let doc = escape_graphql_string(&doc);
        crate::triggers::callback::test_agent_access(&node)
            .execute(&format!(
                "{{AgentRequest(filter:{{_docID:{{_eq:\"{doc}\"}}}}){{interrupt_requested_at}}}}"
            ))
            .await
            .unwrap()["data"]["AgentRequest"][0]["interrupt_requested_at"]
            .clone()
    };
    // A key without a managed Agent signer, even one derived by this runtime,
    // is refused before any read or write.
    let root = crate::triggers::callback::test_root_identity();
    let unmanaged = crate::store::identity::derive_agent_identity(&root, &"f".repeat(64)).unwrap();
    let refused = interrupt_request(&node, unmanaged.as_ref(), "logical-latch").await;
    assert!(matches!(
        refused
            .unwrap_err()
            .downcast_ref::<crate::store::config_client::ConfigClientError>(),
        Some(crate::store::config_client::ConfigClientError::ManagedAgentSignerRequired)
    ));
    assert!(latched().await.is_null());
    interrupt_request(&node, agent.as_ref(), "logical-latch")
        .await
        .unwrap();
    assert!(latched().await.is_string());
    node.shutdown().await.expect("shut down test node");
}
