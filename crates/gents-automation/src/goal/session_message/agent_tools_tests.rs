use super::SessionGoals;
use gents_protocol::request_lifecycle::{SessionMessageCause, SessionMessageTarget};
use gents_protocol::tool_call::AwaitMode;
use gents_session::session::message::plan::plan;
use gents_session::session::message::{
    commit, plan_with_delivery, stage_planned_write_in_txn, AgentMessageDelivery, RenderedBody,
};
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::call::lifecycle::admission_fixture::{
    cause, owner, set_state, signer, PublishedAdmissionOptions, PublishedSessionMessage,
};

async fn started(
    name: &str,
) -> (
    PublishedSessionMessage,
    gents_protocol::row::AgentRequest,
    gents_protocol::row::AgentRequest,
) {
    gents_toolcalls::call::lifecycle::admission_fixture::started(
        name,
        crate::lifecycle_hooks::composition(),
    )
    .await
}

/// Goal rows carry document ACP, so their signed blocks are read as the
/// owning Agent; the node's own caller holds no relation to them.
async fn signer_as_actor(node: &defra_node::Node, actor: &str, cid: &str) -> String {
    let (block, signature) = node
        .authorized_signed_block_bytes(cid, Some(actor))
        .await
        .unwrap();
    let block = defra_core::block::Block::from_dag_cbor(&block).unwrap();
    let signature = defra_core::block::Signature::from_dag_cbor(&signature).unwrap();
    db::block::verify::verified_signature_signer_did(&block, &signature).unwrap()
}

/// Goal rows are Agent-owned, so the calling Agent reads the canonical goal.
async fn canonical_goal(
    message: &PublishedSessionMessage,
    node_did: &str,
    session_id: &str,
) -> Option<crate::goal::GoalDocument> {
    let (node_did, session_id) = (node_did.to_owned(), session_id.to_owned());
    owner(message)
        .transact_readonly("test.canonical_goal", move |txn| {
            let (node_did, session_id) = (node_did.clone(), session_id.clone());
            Box::pin(async move {
                crate::goal::load_canonical_goal_in_txn(txn, &node_did, &session_id).await
            })
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn interrupt_delivery_latches_only_the_prepublication_head() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().interrupt_delivery_cases;
    for case in cases {
        let (message, caller, caused) = started("interrupt-delivery-atomic").await;
        let node = message.admission.node.clone();
        set_state(
            &owner(&message),
            &caused.doc_id,
            if case.planned.is_some() {
                "processing"
            } else {
                "completed"
            },
        )
        .await;
        crate::goal::seed_goal_from_access(
            &message.admission.tool.access().unwrap(),
            &caused.node_did,
            &caused.session_id,
            Some("interrupt goal"),
            Some(gents_protocol::goal_status::GoalStatus::Active),
            None,
            None,
        )
        .await
        .unwrap();
        let cause = cause(&message, caller.request_hop).await;
        let target = SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_did: caused.agent_did.clone(),
            session_id: caused.session_id.clone(),
        };
        let plan = plan_with_delivery(
            &node,
            &SessionGoals,
            &cause,
            &target,
            RenderedBody {
                content: "deliver after interruption".to_owned(),
                goal: None,
            },
            None,
            AgentMessageDelivery::Interrupt,
            &signer(&message),
        )
        .await
        .unwrap()
        .unwrap();
        let replacement = if case.active == Some(2) {
            let replacement_plan = plan_with_delivery(
                &node,
                &SessionGoals,
                &cause,
                &target,
                RenderedBody {
                    content: "replacement active request".to_owned(),
                    goal: None,
                },
                None,
                AgentMessageDelivery::Queue,
                &signer(&message),
            )
            .await
            .unwrap()
            .unwrap();
            let receipt = message
                .admission
                .tool
                .access()
                .unwrap()
                .transact_write("test.replacement", |txn| {
                    let (cause, plan) = (&cause, &replacement_plan);
                    Box::pin(async move {
                        stage_planned_write_in_txn(txn, &SessionGoals, cause, plan).await
                    })
                })
                .await
                .unwrap();
            Some(receipt.doc_id)
        } else {
            None
        };
        set_state(
            &owner(&message),
            &caused.doc_id,
            if case.active == Some(1) {
                "processing"
            } else {
                "completed"
            },
        )
        .await;
        if let Some(doc_id) = &replacement {
            set_state(&owner(&message), doc_id, "processing").await;
        }
        let receipt = message
            .admission
            .tool
            .access()
            .unwrap()
            .transact_write("test.interrupt_delivery", |txn| {
                let (cause, plan) = (&cause, &plan);
                Box::pin(async move {
                    stage_planned_write_in_txn(txn, &SessionGoals, cause, plan).await
                })
            })
            .await
            .unwrap();
        let response = owner(&message).execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID interrupt_requested_at }} }}"#,
            escape_graphql_string(&target.session_id),
        )).await.unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        let old = rows
            .iter()
            .find(|row| row["_docID"] == caused.doc_id)
            .unwrap();
        let new = rows
            .iter()
            .find(|row| row["_docID"] == receipt.doc_id)
            .unwrap();
        let second = replacement
            .as_ref()
            .map(|doc| rows.iter().find(|row| row["_docID"] == *doc).unwrap());
        let actual = if old["interrupt_requested_at"].as_str().is_some() {
            Some(1)
        } else if second.is_some_and(|row| row["interrupt_requested_at"].as_str().is_some()) {
            Some(2)
        } else {
            None
        };
        assert_eq!(actual, case.expected, "{case:?}, rows={rows:?}");
        let goal = canonical_goal(&message, &caused.node_did, &caused.session_id)
            .await
            .unwrap();
        assert_eq!(
            goal.parsed_status() == Some(gents_protocol::goal_status::GoalStatus::Paused),
            case.goal_paused,
            "{case:?}"
        );
        assert!(
            new["interrupt_requested_at"].is_null(),
            "{case:?}, rows={rows:?}"
        );
        node.shutdown().await.expect("shut down test node");
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }
}

#[tokio::test]
async fn agent_new_goal_publishes_for_target_with_the_caller_as_native_owner() {
    use gents_toolcalls::call::lifecycle::admission_fixture::published_admission_with_owner;

    let modeled = tokio::task::spawn_blocking(|| {
        gents_lean_vocab::lean_goal_submission_cases()
            .iter()
            .find(|case| case.name == "commit_both_atomically")
            .unwrap()
            .clone()
    })
    .await
    .unwrap();
    let (mut admission, request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "agent-new-goal-atomic".into(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        start_running: false,
        tool_name: Some(gents_protocol::tool_names::AGENT_NEW_TOOL_NAME.into()),
        composition: crate::lifecycle_hooks::composition(),
        ..Default::default()
    })
    .await
    .unwrap();
    let node = admission.node.clone();
    let caller = request.request();
    let caller_access = admission.tool.access().unwrap();
    let signer = caller_access.managed_agent_signer().unwrap();
    let root =
        gents_store::identity::KeyIdentity::load_existing(admission.path.join("node.key"), None)
            .unwrap();
    let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
    let target = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    // Agent configuration is authored by the User; a managed Agent cannot
    // publish another Agent's configuration document.
    gents_inference::test_support::install_test_agent_binding_with_access(
        &caller.node_did,
        "nested Goal target",
        &binding,
        &admission.client,
    )
    .await;
    let target_rows = admission
        .client
        .execute(&format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
            escape_graphql_string(&binding.agent_did)
        ))
        .await
        .unwrap();
    let target_doc = target_rows["data"]["Agent"][0]["_docID"]
        .as_str()
        .expect("installed nested Goal target")
        .to_owned();
    for (relation, actor) in [
        ("session_writer", caller.agent_did.as_str()),
        ("executor", target.did()),
    ] {
        admission
            .client
            .add_actor_relationship(
                "Agent",
                &target_doc,
                relation,
                &identity::Did::new(actor).unwrap(),
            )
            .await
            .unwrap();
    }

    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_did: caller.agent_did.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_requester_did: caller.requester_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: admission.tool.tool_call_id().to_owned(),
        tool_call_doc_id: admission.tool.doc_id().unwrap().to_owned(),
        correlation: None,
    };
    let target_scope = SessionMessageTarget {
        node_did: caller.node_did.clone(),
        agent_did: target.did().to_owned(),
        session_id: uuid::Uuid::new_v4().to_string(),
    };
    let mut plan = plan(
        &node,
        &SessionGoals,
        &cause,
        &target_scope,
        RenderedBody {
            content: "start nested work".into(),
            goal: Some(("finish nested work".into(), Some(500))),
        },
        None,
        false,
        &signer,
    )
    .await
    .unwrap()
    .unwrap();
    plan.sign_agent_creation(&cause.caller_node_did, &caller.input, &signer)
        .await
        .unwrap();
    let receipt = commit(
        &node,
        &SessionGoals,
        &cause,
        &mut admission.tool,
        plan,
        false,
    )
    .await
    .unwrap();
    let session = escape_graphql_string(&receipt.session_id);
    let rows = caller_access.execute(&format!(r#"{{
        AgentSession(filter: {{session_id: {{_eq: "{session}"}}}}) {{_docID agent_did requester_did provenance}}
        AgentRequest(filter: {{session_id: {{_eq: "{session}"}}}}) {{_docID lifecycle_state agent_did}}
        Goal(filter: {{session_id: {{_eq: "{session}"}}}}) {{_docID session_doc_id agent_did creator_did parent_linked assignment_root_request_doc_id}}
        GoalCreationClaim(filter: {{session_id: {{_eq: "{session}"}}}}) {{_docID session_doc_id agent_did creator_did parent_linked}}
    }}"#)).await.unwrap();
    let sessions = rows["data"]["AgentSession"].as_array().unwrap();
    let requests = rows["data"]["AgentRequest"].as_array().unwrap();
    let goals = rows["data"]["Goal"].as_array().unwrap();
    let claims = rows["data"]["GoalCreationClaim"].as_array().unwrap();
    assert_eq!(!goals.is_empty(), modeled.expected_durable_goal);
    assert_eq!(!requests.is_empty(), modeled.expected_runnable_request);
    assert_eq!(sessions.len(), 1);
    assert_eq!(requests.len(), 1);
    assert_eq!(goals.len(), 1);
    assert_eq!(claims.len(), 1);
    assert_eq!(sessions[0]["agent_did"], target.did());
    assert_eq!(sessions[0]["requester_did"], caller.agent_did);
    assert_eq!(requests[0]["agent_did"], target.did());
    assert_eq!(requests[0]["_docID"], receipt.request_doc_id);
    assert_eq!(
        goals[0]["assignment_root_request_doc_id"],
        receipt.request_doc_id
    );
    let target_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(target.did()).unwrap(),
        target.as_ref(),
    )
    .unwrap();
    let publication = tokio::task::spawn_blocking(|| {
        gents_lean_vocab::lean_contract_snapshot()
            .delegated_publication_cases
            .iter()
            .find(|case| case["name"] == "published")
            .unwrap()
            .clone()
    })
    .await
    .unwrap();
    let creation: gents_protocol::session::SessionProvenance =
        serde_json::from_value(sessions[0]["provenance"].clone()).unwrap();
    let creation = creation.creation_receipt.unwrap();
    assert!(gents_store::identity::verify_did_signature(
        &caller.agent_did,
        &creation.signing_payload(),
        &creation.signature
    )
    .unwrap());
    assert!(matches!(
        gents_store::identity::verify_did_signature(
            target.did(),
            &creation.signing_payload(),
            &creation.signature
        ),
        Err(gents_store::identity::IdentityError::VerifyPayload { did, .. })
            if did == target.did()
    ));
    for (collection, row) in [
        ("AgentSession", &sessions[0]),
        ("AgentRequest", &requests[0]),
    ] {
        let doc = row["_docID"].as_str().unwrap().to_owned();
        let owners = caller_access
            .transact_readonly("test.delegation_owner", |txn| {
                let doc = &doc;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc))
                        .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0].owner.as_ref().map(|owner| owner.as_str()),
            Some(caller.agent_did.as_str())
        );
        let doc = escape_graphql_string(&doc);
        let view = target_access
            .execute(&format!(
                r#"{{ {collection}(filter: {{_docID: {{_eq: "{doc}"}}}}) {{_docID}} }}"#
            ))
            .await
            .unwrap();
        assert_eq!(
            !view["data"][collection].as_array().unwrap().is_empty(),
            publication["expected_target_read"].as_bool().unwrap()
        );
    }
    let goal_links = tokio::task::spawn_blocking(|| {
        gents_lean_vocab::lean_contract_snapshot()
            .goal_link_cases
            .clone()
    })
    .await
    .unwrap();
    for (collection, child) in [("Goal", &goals[0]), ("GoalCreationClaim", &claims[0])] {
        let modeled_link = goal_links
            .iter()
            .find(|case| {
                case.collection == collection
                    && case.actor == "agent"
                    && case.mode == "fresh"
                    && case.commit
            })
            .unwrap();
        assert_eq!(child["session_doc_id"], sessions[0]["_docID"]);
        assert_eq!(child["agent_did"], target.did());
        assert_eq!(child["creator_did"], caller.agent_did);
        assert_eq!(child["parent_linked"], modeled_link.initial.linked);
        let doc = child["_docID"].as_str().unwrap().to_owned();
        let owners = caller_access
            .transact_readonly("test.nested_goal_owner", |txn| {
                let doc = &doc;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc))
                        .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            owners[0].owner.as_ref().map(|owner| owner.as_str()),
            Some(caller.agent_did.as_str())
        );
        let doc = escape_graphql_string(&doc);
        let target_view = target_access
            .execute(&format!(
                r#"{{ {collection}(filter: {{_docID: {{_eq: "{doc}"}}}}) {{_docID}} }}"#
            ))
            .await
            .unwrap();
        assert_eq!(target_view["data"][collection].as_array().unwrap().len(), 1);
        let history = caller_access.execute(&format!(r#"{{ _commits(docID: "{doc}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#)).await.unwrap();
        let cid = history["data"]["_commits"][0]["cid"].as_str().unwrap();
        assert_eq!(
            signer_as_actor(&node, &caller.agent_did, cid).await,
            caller.agent_did
        );
    }
    node.shutdown().await.unwrap();
    std::fs::remove_dir_all(&admission.path).unwrap();
}

#[tokio::test]
async fn an_idle_same_node_interrupt_pauses_the_goal() {
    let (message, _, caused) = started("agent-interrupt-steer").await;
    let node = message.admission.node.clone();
    let scope = gents_protocol::session::SessionScope::of_request(&caused);
    crate::goal::seed_goal_from_access(
        &message.admission.tool.access().unwrap(),
        &caused.node_did,
        &caused.session_id,
        Some("pause after operator interrupt"),
        Some(gents_protocol::goal_status::GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        gents_session::session::message::interrupt_session_with_access(
            &node,
            &owner(&message),
            &scope
        )
        .await
        .unwrap(),
        None
    );
    assert_eq!(
        canonical_goal(&message, &caused.node_did, &caused.session_id)
            .await
            .unwrap()
            .parsed_status(),
        Some(gents_protocol::goal_status::GoalStatus::Paused),
        "an idle same-node interrupt pauses autonomous Goal work"
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn explicit_stop_pauses_the_goal() {
    let (message, _, caused) = started("agent-stop-drains").await;
    let node = message.admission.node.clone();
    crate::goal::seed_goal_from_access(
        &message.admission.tool.access().unwrap(),
        &caused.node_did,
        &caused.session_id,
        Some("stop this session"),
        Some(gents_protocol::goal_status::GoalStatus::Active),
        None,
        None,
    )
    .await
    .unwrap();
    let receipt = gents_session::session::stop_session_with_access(
        &node,
        &owner(&message),
        &caused.node_did,
        &caused.session_id,
        caused.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.status.as_str(), "stop_requested");
    assert_eq!(
        canonical_goal(&message, &caused.node_did, &caused.session_id)
            .await
            .unwrap()
            .parsed_status(),
        Some(gents_protocol::goal_status::GoalStatus::Paused)
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
