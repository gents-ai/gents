use defra_node::Node as EmbeddedNode;
use gents_session::request::interrupt::*;
use gents_store::graphql::escape_graphql_string;
use gents_store::test_support::{
    fixture_agent, fixture_agent_access, fixture_agent_did, fixture_root, FIXTURE_AGENT_NONCE,
};
use serde_json::{json, Value};
use std::sync::Arc;

#[tokio::test]
async fn generated_interrupt_drain_preserves_foreign_owner_and_requester_queues() {
    for case in gents_lean_vocab::families::contracts_queue_deadline_interrupt_scope::cases() {
        let input = &case.input;
        let db = crate::triggers::callback::test_node_builder(
            "generated-interrupt-drain-preserves-fore",
        )
        .await;
        let node = db.node.clone();
        let root_did = gents_store::test_support::PIN_FIXED_DID;
        crate::triggers::callback::seed_test_agent(&node, root_did).await;
        let agent = fixture_agent(FIXTURE_AGENT_NONCE);
        let signer = agent.managed_agent_signer().unwrap();
        let signer = Some(&signer);
        let session = input.session_id.to_string();
        use gents_store::identity::NodeIdentity as _;
        let root = fixture_root();
        let derived_did = |nonce: char| fixture_agent_did(&nonce.to_string().repeat(64));
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
            let agent = fixture_agent_did(FIXTURE_AGENT_NONCE);
            let input=source.map(|source|json!({"queue":{"source":source,"policy":if source=="background_completion" {"coalesce"} else {"append"},"key":if source=="background_completion" {Some(format!("background_completion:{session}"))}else{None}}}));
            let value = json!({"request_id":id,"purpose":"normal","node_did":owner,"requester_did":requester,"session_id":session,
            "agent_did":agent,"content":format!("request {id}"),"lifecycle_state":if source.is_none() && requester.is_none() && id == "same-parent-label" {"processing"} else {"pending"},"interrupt_generation":0,"execution_origin":if source.is_some(){"scheduled"}else{"interactive"},"input":input,"created_at":"2026-09-01T00:00:00Z"});
            let root = ::identity::Did::new(gents_store::test_support::PIN_FIXED_DID).unwrap();
            gents_store::config_client::ConfigAccess::transact_local_write(
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
            let response = fixture_agent_access(node, FIXTURE_AGENT_NONCE)
            .execute(&format!("{{AgentRequest(filter:{{_docID:{{_eq:\"{doc}\"}}}}){{lifecycle_state}}}}"))
            .await
            .unwrap();
            response["data"]["AgentRequest"][0].clone()
        }
        /// The session `(owner, session)` and its interrupt marker, created by
        /// the runtime root with the fixture Agent as its working Agent.
        async fn create_session(node: &Arc<EmbeddedNode>, owner: &str, session: &str) {
            let agent = fixture_agent_did(FIXTURE_AGENT_NONCE);
            let root = ::identity::Did::new(gents_store::test_support::PIN_FIXED_DID).unwrap();
            gents_store::config_client::ConfigAccess::transact_local_write(
            node, Some(root), "test.interrupt.scope_session", |txn| { let agent = agent.clone(); Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                    &json!({"input": {"node_did": owner, "session_id": session, "agent_did": agent, "created_at": "2026-09-01T00:00:00Z"}}),
                ).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentSession")?;
                txn.add_actor_relationship("AgentSession", &doc, "working_agent", &::identity::Did::new(&agent)?).await?;
                gents_session::session::interrupt_marker::create_in_txn(txn, &doc, owner, session).await?;
                Ok::<_, anyhow::Error>(())
            }) },
        ).await.unwrap()
        }
        /// When the interrupt of its session passed the request `doc`.
        async fn interrupted_at(node: &Arc<EmbeddedNode>, doc: &str) -> Option<String> {
            request_interrupted_at_by_doc_id_with_access(
                &fixture_agent_access(node, FIXTURE_AGENT_NONCE),
                doc,
            )
            .await
            .unwrap()
        }
        create_session(&node, owner, &session).await;
        create_session(&node, foreign_owner, &session).await;
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
            &input.automated_request_ids[0].to_string(),
            owner,
            None,
            &session,
            Some("background_completion"),
        )
        .await;
        let user = create(
            &node,
            &input.user_request_ids[0].to_string(),
            owner,
            None,
            &session,
            Some("user"),
        )
        .await;
        let foreign_requester = create(
            &node,
            &input.foreign_requester_request_ids[0].to_string(),
            owner,
            Some(requester),
            &session,
            Some("background_completion"),
        )
        .await;
        let foreign = create(
            &node,
            &input.foreign_owner_request_ids[0].to_string(),
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
                .downcast_ref::<gents_store::config_client::ConfigClientError>(),
            Some(gents_store::config_client::ConfigClientError::ManagedAgentSignerRequired)
        ));
        assert_eq!(interrupted_at(&node, &parent).await, None);
        interrupt_request_by_doc_id(&node, signer, &parent, owner, None)
            .await
            .unwrap();
        assert_eq!(interrupted_at(&node, &foreign_parent).await, None);
        for doc in [&own, &user] {
            let request = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
                &fixture_agent_access(&node, FIXTURE_AGENT_NONCE),
                doc,
            )
            .await
            .unwrap()
            .unwrap();
            let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_node_did(
                node.clone(),
                request,
                60,
            );
            lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
            assert_ne!(
                lifecycle.claim_with_identity().await.unwrap(),
                gents_session::request::lifecycle::ClaimOutcome::Claimed,
                "the interrupted parent still runs"
            );
        }
        let mut states = Vec::new();
        for (id, doc) in [
            (input.automated_request_ids[0], &own),
            (input.user_request_ids[0], &user),
            (input.foreign_requester_request_ids[0], &foreign_requester),
            (input.foreign_owner_request_ids[0], &foreign),
        ] {
            let row = row(&node, doc).await;
            states.push((id, row["lifecycle_state"].as_str().unwrap().to_owned()));
        }
        let with_state = |ids: &[u64], state: &str| {
            ids.iter()
                .copied()
                .filter(|id| {
                    states
                        .iter()
                        .any(|(seen, observed)| seen == id && observed == state)
                })
                .collect::<Vec<_>>()
        };
        let own_ids = [
            input.automated_request_ids.clone(),
            input.user_request_ids.clone(),
        ]
        .concat();
        let observed = gents_lean_vocab::families::contracts_queue_deadline_interrupt_scope::InterruptScopeOutput {
        automated_drained_request_ids: with_state(&own_ids, "interrupted"),
        preserved_user_pending_request_ids: with_state(&input.user_request_ids, "pending"),
        preserved_foreign_requester_request_ids: with_state(
            &input.foreign_requester_request_ids,
            "pending",
        ),
        preserved_foreign_owner_request_ids: with_state(&input.foreign_owner_request_ids, "pending"),
    };
        let first = interrupted_at(&node, &parent).await;
        assert!(first.is_some());
        interrupt_request_by_doc_id(&node, signer, &parent, owner, None)
            .await
            .unwrap();
        assert_eq!(
            interrupted_at(&node, &parent).await,
            first,
            "a repeat interrupt of an interrupted request writes nothing"
        );
        node.shutdown().await.expect("shut down test node");
        case.check(observed);
    }
}

#[tokio::test]
async fn logical_interrupt_latches_only_as_the_signed_working_agent() {
    let db =
        crate::triggers::callback::test_node_builder("logical-interrupt-latches-only-as-the-si")
            .await;
    let node = db.node.clone();
    let root_did = gents_store::test_support::PIN_FIXED_DID;
    crate::triggers::callback::seed_test_agent(&node, root_did).await;
    let agent = fixture_agent(FIXTURE_AGENT_NONCE);
    let value = json!({"request_id":"logical-latch","purpose":"normal","node_did":root_did,
        "session_id":"logical-latch-session","agent_did":agent.did(),"lifecycle_state":"pending",
        "execution_origin":"interactive","created_at":"2026-09-01T00:00:00Z"});
    let agent_did = agent.did().to_owned();
    let doc = gents_store::config_client::ConfigAccess::transact_local_write(
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
    let state = || async {
        let doc = escape_graphql_string(&doc);
        fixture_agent_access(&node, FIXTURE_AGENT_NONCE)
            .execute(&format!(
                "{{AgentRequest(filter:{{_docID:{{_eq:\"{doc}\"}}}}){{lifecycle_state}}}}"
            ))
            .await
            .unwrap()["data"]["AgentRequest"][0]["lifecycle_state"]
            .clone()
    };
    // A key without a managed Agent signer, even one derived by this runtime,
    // is refused before any read or write.
    let root = fixture_root();
    let unmanaged = gents_store::identity::derive_agent_identity(&root, &"f".repeat(64)).unwrap();
    let refused = interrupt_request(&node, unmanaged.as_ref(), "logical-latch").await;
    assert!(matches!(
        refused
            .unwrap_err()
            .downcast_ref::<gents_store::config_client::ConfigClientError>(),
        Some(gents_store::config_client::ConfigClientError::ManagedAgentSignerRequired)
    ));
    assert_eq!(state().await, "pending");
    interrupt_request(&node, agent.as_ref(), "logical-latch")
        .await
        .unwrap();
    assert_eq!(state().await, "interrupted", "a pending request is interrupted before its claim");
    node.shutdown().await.expect("shut down test node");
}
