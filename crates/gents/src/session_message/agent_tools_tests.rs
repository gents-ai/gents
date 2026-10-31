use super::target::session_target_allowed;
use super::*;
use crate::tool_call_lifecycle::admission_fixture::{
    published_session_message, PublishedAdmissionOptions, PublishedSessionMessage,
};
use crate::tool_call_lifecycle::AwaitMode;

pub(super) async fn started(
    name: &str,
) -> (
    PublishedSessionMessage,
    crate::AgentRequest,
    crate::AgentRequest,
) {
    let message = published_session_message(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let caller = crate::request_binding::load_agent_request_by_doc_id(
        &node,
        message.admission.tool.request_doc_id().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    let caused =
        crate::request_binding::load_agent_request_by_doc_id(&node, &message.caused_request_doc_id)
            .await
            .unwrap()
            .unwrap();
    materialize_session(&node, &caused).await;
    (message, caller, caused)
}

async fn materialize_session(node: &std::sync::Arc<EmbeddedNode>, caused: &crate::AgentRequest) {
    materialize_session_at(node, caused, &chrono::Utc::now().to_rfc3339()).await;
}

async fn materialize_session_at(
    node: &std::sync::Arc<EmbeddedNode>,
    caused: &crate::AgentRequest,
    created_at: &str,
) {
    let caused = caused.clone();
    let created_at = created_at.to_owned();
    ConfigAccess::transact_local_write(node, None, "test.materialize_session", move |txn| {
        let caused = caused.clone();
        let created_at = created_at.clone();
        Box::pin(async move {
            crate::session::ensure_session_in_txn(
                txn,
                &caused.session_id,
                &caused.node_did,
                &caused.agent_id,
                caused.requester_did.as_deref(),
                None,
                crate::lifecycle::materialize::created_session_provenance(&caused),
                &created_at,
            )
            .await
        })
    })
    .await
    .unwrap();
}

async fn set_state(node: &EmbeddedNode, doc_id: &str, state: &str) {
    let response = node
        .execute(&format!(
            r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ lifecycle_state: "{state}" }}) {{ _docID }} }}"#,
            escape_graphql_string(doc_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
}

#[tokio::test]
async fn steer_reachability_matches_lean() {
    use gents_protocol::request_input::{QueueDelivery, QueuePolicy, QueueSource, RequestQueue};
    let (message, _, head) = started("steer-reachability-model").await;
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().agent_steer_reachability_cases;
    for case in cases {
        let rows = case
            .inputs
            .iter()
            .map(|item| {
                let (delivery, source, agent) = match item.kind.as_str() {
                    "queued" => (QueueDelivery::Queue, QueueSource::User, false),
                    "user_steer" => (QueueDelivery::Steer, QueueSource::User, false),
                    "agent_steer" => (QueueDelivery::Steer, QueueSource::Steering, true),
                    "blocked" => (
                        QueueDelivery::Queue,
                        QueueSource::BackgroundCompletion,
                        false,
                    ),
                    other => panic!("unexpected modeled boundary kind: {other}"),
                };
                let mut input = head.input.clone();
                input.cwd = if item.compatible {
                    head.input.cwd.clone()
                } else {
                    Some("/incompatible".into())
                };
                input.queue = Some(RequestQueue {
                    delivery,
                    source,
                    policy: QueuePolicy::Append,
                    position: None,
                    key: None,
                    queued_after_request_id: agent.then(|| head.request_id.clone()),
                    interrupted_request_id: None,
                    background_completion_wake_version: None,
                });
                gents_protocol::row::AgentRequestRow {
                    doc_id: Some(item.id.to_string()),
                    request_id: item.id.to_string(),
                    node_did: Some(head.node_did.clone()),
                    session_id: Some(head.session_id.clone()),
                    requester_did: head.requester_did.clone(),
                    agent_id: Some(head.agent_id.clone()),
                    purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
                    execution_origin: Some("interactive".into()),
                    caused_by_parent_request_doc_id: agent.then(|| "parent".into()),
                    caused_by_parent_tool_call_doc_id: agent.then(|| "tool".into()),
                    input: Some(input),
                    ..Default::default()
                }
            })
            .collect();
        let actual = crate::lifecycle::queue::steer_reachable(
            &head,
            rows,
            &case.target.to_string(),
            chrono::Utc::now(),
        )
        .is_ok();
        assert_eq!(actual, case.expected, "{case:?}");
    }
    message.admission.node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn interrupt_delivery_latches_only_the_prepublication_head() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().interrupt_delivery_cases;
    for case in cases {
        let (message, caller, caused) = started("interrupt-delivery-atomic").await;
        let node = message.admission.node.clone();
        set_state(
            &node,
            &caused.doc_id,
            if case.planned.is_some() {
                "processing"
            } else {
                "completed"
            },
        )
        .await;
        crate::goal::set_goal(
            &node,
            &caused.node_did,
            &caused.session_id,
            Some("interrupt goal"),
            Some(crate::goal::GoalStatus::Active),
            None,
        )
        .await
        .unwrap();
        let cause = super::hop_tests::cause(&message, caller.request_hop);
        let target = SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            session_id: caused.session_id.clone(),
        };
        let plan = plan_with_delivery(
            &node,
            &cause,
            &target,
            RenderedBody {
                content: "deliver after interruption".to_owned(),
                goal: None,
            },
            None,
            AgentMessageDelivery::Interrupt,
        )
        .await
        .unwrap()
        .unwrap();
        let replacement = if case.active == Some(2) {
            let replacement_plan = plan_with_delivery(
                &node,
                &cause,
                &target,
                RenderedBody {
                    content: "replacement active request".to_owned(),
                    goal: None,
                },
                None,
                AgentMessageDelivery::Queue,
            )
            .await
            .unwrap()
            .unwrap();
            let receipt =
                ConfigAccess::transact_local_write(&node, None, "test.replacement", |txn| {
                    let (cause, plan) = (&cause, &replacement_plan);
                    Box::pin(async move { stage_planned_write_in_txn(txn, cause, plan).await })
                })
                .await
                .unwrap();
            Some(receipt.doc_id)
        } else {
            None
        };
        set_state(
            &node,
            &caused.doc_id,
            if case.active == Some(1) {
                "processing"
            } else {
                "completed"
            },
        )
        .await;
        if let Some(doc_id) = &replacement {
            set_state(&node, doc_id, "processing").await;
        }
        let receipt =
            ConfigAccess::transact_local_write(&node, None, "test.interrupt_delivery", |txn| {
                let (cause, plan) = (&cause, &plan);
                Box::pin(async move { stage_planned_write_in_txn(txn, cause, plan).await })
            })
            .await
            .unwrap();
        let response = ConfigAccess::local(node.clone()).execute(&format!(
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
        let goal = crate::goal::load_canonical_goal(&node, &caused.node_did, &caused.session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            goal.parsed_status() == Some(crate::goal::GoalStatus::Paused),
            case.goal_paused,
            "{case:?}"
        );
        assert!(
            new["interrupt_requested_at"].is_null(),
            "{case:?}, rows={rows:?}"
        );
        node.shutdown().await;
        std::fs::remove_dir_all(&message.admission.path).unwrap();
    }
}

#[tokio::test]
async fn explicit_steer_checks_the_physical_head_at_publication() {
    let (message, caller, caused) = started("agent-steer-head-change").await;
    let node = message.admission.node.clone();
    set_state(&node, &caused.doc_id, "processing").await;
    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_id: caller.agent_id.clone(),
        caller_session_id: caller.session_id.clone(),
        caller_requester_did: caller.requester_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: "steer-tool".to_owned(),
        tool_call_doc_id: "steer-tool-doc".to_owned(),
        correlation: None,
    };
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_id: caused.agent_id.clone(),
        session_id: caused.session_id.clone(),
    };
    let make_plan = || async {
        plan_with_delivery(
            &node,
            &cause,
            &target,
            RenderedBody {
                content: "change course".to_owned(),
                goal: None,
            },
            None,
            AgentMessageDelivery::Steer,
        )
        .await
        .unwrap()
        .unwrap()
    };
    let stale = make_plan().await;
    set_state(&node, &caused.doc_id, "completed").await;
    let (cause_ref, stale_ref) = (&cause, &stale);
    let result =
        ConfigAccess::transact_local_write(&node, None, "test.steer.changed_head", move |txn| {
            Box::pin(async move { stage_planned_write_in_txn(txn, cause_ref, stale_ref).await })
        })
        .await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed while the steer was planned"));
    set_state(&node, &caused.doc_id, "processing").await;
    let queued = plan_with_delivery(
        &node,
        &cause,
        &target,
        RenderedBody {
            content: "queued turn".to_owned(),
            goal: None,
        },
        None,
        AgentMessageDelivery::Queue,
    )
    .await
    .unwrap()
    .unwrap();
    let (cause_ref, queued_ref) = (&cause, &queued);
    let queued_receipt = ConfigAccess::transact_local_write(
        &node,
        None,
        "test.steer.queued_predecessor",
        move |txn| {
            Box::pin(async move { stage_planned_write_in_txn(txn, cause_ref, queued_ref).await })
        },
    )
    .await
    .unwrap();
    let current = make_plan().await;
    let (cause_ref, current_ref) = (&cause, &current);
    let receipt =
        ConfigAccess::transact_local_write(&node, None, "test.steer.compatible", move |txn| {
            Box::pin(async move { stage_planned_write_in_txn(txn, cause_ref, current_ref).await })
        })
        .await
        .expect("compatible steer is published");
    let row = crate::request_binding::load_agent_request_by_doc_id(&node, &receipt.doc_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.input.queue.unwrap().delivery,
        gents_protocol::request_input::QueueDelivery::Steer
    );
    let (node_did, session_id) = (&caused.node_did, &caused.session_id);
    let snapshot = crate::config_client::ConfigAccess::transact_local_readonly(
        &node,
        None,
        "test.steer.predecessor_order",
        move |txn| {
            Box::pin(async move {
                crate::lifecycle::queue::pending_rows_in_txn(txn, node_did, session_id).await
            })
        },
    )
    .await
    .unwrap();
    assert!(snapshot
        .iter()
        .any(|row| row.doc_id.as_deref() == Some(queued_receipt.doc_id.as_str())));
    let candidate = snapshot
        .iter()
        .find(|row| row.doc_id.as_deref() == Some(receipt.doc_id.as_str()))
        .unwrap();
    let mut contexts = Vec::new();
    let mut human = caused.clone();
    human.requester_did = Some("did:test:human".to_owned());
    contexts.push(("human requester", human));
    let mut cwd = caused.clone();
    cwd.input.cwd = Some("/different-cwd".to_owned());
    contexts.push(("cwd", cwd));
    let mut skills = caused.clone();
    skills
        .input
        .selected_skill_ids
        .push("different-skill".to_owned());
    contexts.push(("skills", skills));
    let mut workspace = caused.clone();
    workspace.workspace_id = Some("different-workspace".to_owned());
    contexts.push(("workspace", workspace));
    let mut goal = caused.clone();
    goal.input.goal_assignment = Some(gents_protocol::request_input::GoalAssignmentInput {
        goal_id: "different-goal".to_owned(),
        root_request_doc_id: None,
    });
    contexts.push(("goal", goal));
    let modeled = crate::lean_vocab_test::lean_contract_snapshot()
        .agent_steer_disposition_cases
        .iter()
        .find(|case| case.active && !case.compatible)
        .unwrap();
    for case in &crate::lean_vocab_test::lean_contract_snapshot().agent_steer_target_cases {
        let mut head = caused.clone();
        if case["planned_head"] != case["active_head"] {
            head.request_id = "a-later-turn".to_owned();
        }
        assert_eq!(
            crate::lifecycle::queue::steering_compatible(&head, candidate, chrono::Utc::now()),
            case["expected"].as_bool().unwrap(),
            "{case}"
        );
    }
    for (name, head) in contexts {
        let disposition =
            if crate::lifecycle::queue::steering_compatible(&head, candidate, chrono::Utc::now()) {
                "steer"
            } else {
                "refuse"
            };
        assert_eq!(disposition, modeled.expected, "{name}");
    }
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn only_the_starting_session_may_interrupt() {
    let (message, caller, caused) = started("agent-interrupt-permission").await;
    let node = message.admission.node.clone();
    let caller = SessionScope::of_request(&caller);
    let target = SessionScope::of_request(&caused);
    assert!(interrupt_refusal(&node, &caller, &target)
        .await
        .unwrap()
        .is_none());
    let another = SessionScope {
        session_id: "another-session".to_owned(),
        ..caller.clone()
    };
    assert!(interrupt_refusal(&node, &another, &target)
        .await
        .unwrap()
        .is_some());
    assert!(interrupt_refusal(&node, &target, &caller)
        .await
        .unwrap()
        .is_some());
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn signed_creation_waits_for_its_canonical_parent_evidence() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "delayed-agent-creation-evidence".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let caller = crate::request_binding::load_agent_request_by_doc_id(
        &node,
        message.admission.tool.request_doc_id().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    let caused =
        crate::request_binding::load_agent_request_by_doc_id(&node, &message.caused_request_doc_id)
            .await
            .unwrap()
            .unwrap();
    let access = ConfigAccess::local(node.clone());
    let call_doc = message.admission.tool.doc_id().unwrap();
    let set_name = |name: &str| {
        format!(
        "mutation {{update_AgentToolCall(docID: \"{}\", input: {{tool_name: \"{}\"}}) {{_docID}}}}",
        escape_graphql_string(call_doc), escape_graphql_string(name),
    )
    };
    access
        .write("test.hide_creation_cause", &set_name("agent_message"))
        .await
        .unwrap();
    materialize_session(&node, &caused).await;
    let target = SessionScope::of_request(&caused);
    let source = SessionScope::of_request(&caller);
    let session = crate::session_origin::load_session(
        &access,
        &target.session_id,
        target.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(session.provenance.unwrap().creation_receipt.is_some());
    assert!(interrupt_refusal(&node, &source, &target)
        .await
        .unwrap()
        .is_some());
    access
        .write(
            "test.restore_creation_cause",
            &set_name(crate::toolset::AGENT_NEW_TOOL_NAME),
        )
        .await
        .unwrap();
    assert!(interrupt_refusal(&node, &source, &target)
        .await
        .unwrap()
        .is_none());
    let mut forged = caused.input.clone();
    forged.session_creation = Some(gents_protocol::request_input::SessionCreationIntent::PluginJob);
    let mutation = format!("mutation($input: JSON) {{update_AgentRequest(docID: \"{}\", input: {{input: $input}}) {{_docID}}}}", escape_graphql_string(&caused.doc_id));
    let rejected = access
        .transact_write("test.forge_creation_intent", |txn| {
            let mutation = &mutation;
            let forged = &forged;
            Box::pin(async move {
                txn.execute_with_variables(mutation, &serde_json::json!({"input": forged}))
                    .await
            })
        })
        .await
        .unwrap_err();
    assert!(
        rejected.to_string().contains("immutable field"),
        "{rejected:#}"
    );
    assert!(interrupt_refusal(&node, &source, &target)
        .await
        .unwrap()
        .is_none());
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn remote_signed_creation_survives_parent_evidence_arriving_after_claim() {
    use crate::identity::NodeIdentity;
    use crate::tool_call_lifecycle::admission_fixture::{
        materialize_session_message, published_admission_with_owner, test_p2p_config,
    };
    use defra_p2p_adapter::P2pDocumentRequest;
    use std::time::Duration;

    let (mut source, owner) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "remote-delayed-creation".into(),
        real_identity: true,
        p2p: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(crate::toolset::AGENT_NEW_TOOL_NAME.into()),
        start_running: false,
        ..Default::default()
    })
    .await
    .unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let target_identity =
        crate::KeyIdentity::load_or_create(target_dir.path().join("target.key"), None).unwrap();
    let target = Arc::new(
        EmbeddedNode::builder()
            .data_path(target_dir.path())
            .with_node_identity_did(target_identity.did())
            .with_p2p(test_p2p_config())
            .build()
            .await
            .unwrap(),
    );
    crate::ensure_runtime_schemas(&target).await.unwrap();
    let receipt = materialize_session_message(
        &source.node,
        &owner,
        &mut source.tool,
        target_identity.did(),
        "remote work",
    )
    .await
    .unwrap();

    async fn address(node: &EmbeddedNode) -> String {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(address) = node
                    .p2p()
                    .unwrap()
                    .listen_addresses()
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|a| a.starts_with("127.0.0.1:") || a.starts_with("[::1]:"))
                {
                    break address;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("P2P loopback address")
    }
    let source_address = address(&source.node).await;
    let target_address = address(&target).await;
    let mut source_connections = source.node.subscribe(&[events::EventName::PeerConnection]);
    let mut target_connections = target.subscribe(&[events::EventName::PeerConnection]);
    source
        .node
        .p2p()
        .unwrap()
        .connect_peer(&target_address)
        .await
        .unwrap();
    for (node, connections) in [
        (&source.node, &mut source_connections),
        (&target, &mut target_connections),
    ] {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !node
                    .p2p()
                    .unwrap()
                    .connected_peers()
                    .await
                    .unwrap()
                    .is_empty()
                {
                    break;
                }
                connections
                    .recv()
                    .await
                    .expect("peer connection event stream");
            }
        })
        .await
        .expect("P2P peer connection");
    }
    for (node, peer_address) in [(&source.node, &target_address), (&target, &source_address)] {
        node.p2p()
            .unwrap()
            .add_replicator(
                vec!["Network".into()],
                Some(peer_address),
                Default::default(),
                Vec::new(),
                None,
            )
            .await
            .unwrap();
    }
    let peer = target.p2p().unwrap().local_peer_id().await.unwrap();
    async fn push(
        source: &EmbeddedNode,
        target: &Arc<EmbeddedNode>,
        peer: &str,
        collection: &str,
        docs: &[String],
    ) {
        if docs.is_empty() {
            return;
        }
        source
            .p2p()
            .unwrap()
            .push_documents_to_peer(
                peer,
                docs.iter()
                    .map(|doc_id| P2pDocumentRequest {
                        collection: collection.into(),
                        doc_id: doc_id.clone(),
                    })
                    .collect(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let mut present = true;
                for doc in docs {
                    let query = format!(
                        "{{{collection}(filter: {{_docID: {{_eq: \"{}\"}}}}) {{_docID}}}}",
                        escape_graphql_string(doc)
                    );
                    let response = ConfigAccess::local(target.clone())
                        .execute(&query)
                        .await
                        .unwrap();
                    present &= response["data"][collection]
                        .as_array()
                        .is_some_and(|rows| rows.len() == 1);
                }
                if present {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("exact P2P documents arrive");
    }
    push(
        &source.node,
        &target,
        &peer,
        "AgentRequest",
        std::slice::from_ref(&receipt.request_doc_id),
    )
    .await;
    let query = format!(
        "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
        escape_graphql_string(&receipt.request_doc_id),
        crate::watcher::AGENT_REQUEST_FIELDS
    );
    let response = ConfigAccess::local(target.clone())
        .execute(&query)
        .await
        .unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    let mut child = crate::lifecycle::RequestLifecycle::new_with_node_did(
        target.clone(),
        "general",
        target_identity.did(),
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(
        child.claim().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    let caller = SessionScope::of_request(owner.request());
    let child_scope = SessionScope::of_request(child.request());
    assert!(ConfigAccess::local(target.clone())
        .execute("{AgentToolCall {_docID}}")
        .await
        .unwrap()["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(interrupt_refusal(&target, &caller, &child_scope)
        .await
        .unwrap()
        .is_some());

    for collection in [
        "AgentRequest",
        "AgentToolCall",
        "AgentMessage",
        "AgentOutputSegment",
    ] {
        let response = ConfigAccess::local(source.node.clone())
            .execute(&format!("{{{collection} {{_docID}}}}"))
            .await
            .unwrap();
        let docs: Vec<String> = response["data"][collection]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["_docID"].as_str().map(str::to_owned))
            .filter(|doc| doc != &receipt.request_doc_id)
            .collect();
        push(&source.node, &target, &peer, collection, &docs).await;
    }
    assert!(interrupt_refusal(&target, &caller, &child_scope)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        interrupt_session(&target, &child_scope).await.unwrap(),
        Some(child.request().request_id.clone())
    );
    let observed = ConfigAccess::local(target.clone())
        .execute(&format!(
            "{{AgentRequest(docID: \"{}\") {{interrupt_requested_at}}}}",
            escape_graphql_string(&receipt.request_doc_id),
        ))
        .await
        .unwrap();
    assert!(
        observed["data"]["AgentRequest"][0]["interrupt_requested_at"]
            .as_str()
            .is_some()
    );
    target.shutdown().await;
    source.node.shutdown().await;
    std::fs::remove_dir_all(&source.path).unwrap();
}

#[tokio::test]
async fn forged_descriptive_provenance_and_altered_creation_receipt_cannot_grant_control() {
    let (message, caller, caused) = started("agent-interrupt-receipt").await;
    let node = message.admission.node.clone();
    let caller_scope = SessionScope::of_request(&caller);
    let target_scope = SessionScope::of_request(&caused);
    let access = ConfigAccess::local(node.clone());
    let session = crate::session_origin::load_session(
        &access,
        &target_scope.session_id,
        target_scope.requester_did.as_deref(),
    )
    .await
    .unwrap()
    .unwrap();
    let mut forged = session.provenance.unwrap();
    assert!(
        forged.creation_receipt.is_some(),
        "newly materialized session has a receipt"
    );
    forged.creation_receipt.as_mut().unwrap().signature[0] ^= 1;
    let mutation = format!(
        r#"mutation($provenance: JSON) {{update_AgentSession(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}, input: {{provenance: $provenance}}) {{_docID}}}}"#,
        escape_graphql_string(&target_scope.node_did),
        escape_graphql_string(&target_scope.session_id),
    );
    access
        .transact_write("test.corrupt_session_receipt", |txn| {
            let mutation = &mutation;
            let forged = &forged;
            Box::pin(async move {
                txn.execute_with_variables(mutation, &serde_json::json!({"provenance": forged}))
                    .await
            })
        })
        .await
        .unwrap();
    assert!(interrupt_refusal(&node, &caller_scope, &target_scope)
        .await
        .unwrap()
        .is_some());

    let human = "unrelated-human-session";
    let provenance = gents_protocol::session::SessionProvenance {
        parent_request_doc_id: Some(caller.doc_id.clone()),
        opening_request_doc_id: Some(caused.doc_id.clone()),
        ..Default::default()
    };
    let created_at = chrono::Utc::now().to_rfc3339();
    access.transact_write("test.forged_session_provenance", |txn| {
        let provenance = &provenance;
        let caller = &caller;
        let created_at = &created_at;
        Box::pin(async move {
            txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) {create_AgentSession(input: $input) {_docID}}",
                &serde_json::json!({"input": {"node_did": caller.node_did, "session_id": human,
                    "requester_did": caller.node_did, "agent_id": caller.agent_id,
                    "created_at": created_at, "provenance": provenance}}),
            ).await
        })
    }).await.unwrap();
    let human_scope = SessionScope {
        session_id: human.into(),
        ..caller_scope.clone()
    };
    assert!(interrupt_refusal(&node, &caller_scope, &human_scope)
        .await
        .unwrap()
        .is_some());
    let lineage = crate::session_origin::lineage(&access, &caller_scope)
        .await
        .unwrap();
    for scope in [&human_scope, &target_scope] {
        assert!(
            !lineage.started.iter().any(|link| &link.scope == scope),
            "{scope:?}: a parent pointer or an altered receipt is not a verified starter"
        );
    }
    assert_eq!(
        crate::session_origin::started_by(&access, &human_scope)
            .await
            .unwrap(),
        None
    );
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn interrupt_stops_the_turn_and_the_steer_is_a_new_request() {
    let (message, caller, caused) = started("agent-interrupt-steer").await;
    let node = message.admission.node.clone();
    let scope = SessionScope::of_request(&caused);
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_id: caused.agent_id.clone(),
        session_id: caused.session_id.clone(),
    };
    crate::goal::set_goal(
        &node,
        &caused.node_did,
        &caused.session_id,
        Some("pause after operator interrupt"),
        Some(crate::goal::GoalStatus::Active),
        None,
    )
    .await
    .unwrap();
    assert_eq!(interrupt_session(&node, &scope).await.unwrap(), None);
    assert_eq!(
        crate::goal::load_canonical_goal(&node, &caused.node_did, &caused.session_id)
            .await
            .unwrap()
            .unwrap()
            .parsed_status(),
        Some(crate::goal::GoalStatus::Paused),
        "an idle same-node interrupt pauses autonomous Goal work"
    );
    set_state(&node, &message.caused_request_doc_id, "processing").await;

    let cause = SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_id: "caller-agent".to_owned(),
        caller_session_id: "caller-session".to_owned(),
        caller_requester_did: None,
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: "steer-tool".to_owned(),
        tool_call_doc_id: "steer-tool-doc".to_owned(),
        correlation: None,
    };
    let body = || RenderedBody {
        content: "change course".to_owned(),
        goal: None,
    };
    let steering = plan(&node, &cause, &target, body(), None, false)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(steering.delivery(), Delivery::Queued);
    let steer = plan(&node, &cause, &target, body(), None, true)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(steer.delivery(), Delivery::Request);
    assert_eq!(steer.hop(), 1);

    assert_eq!(
        interrupt_session(&node, &scope).await.unwrap(),
        Some(caused.request_id.clone())
    );
    assert!(crate::interrupt::fetch_interrupt_requested_at_by_doc_id(
        &node,
        &message.caused_request_doc_id
    )
    .await
    .unwrap()
    .is_some());
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn explicit_stop_drains_pending_work_and_rejects_late_admission() {
    let (message, caller, caused) = started("agent-stop-drains").await;
    let node = message.admission.node.clone();
    crate::goal::set_goal(
        &node,
        &caused.node_did,
        &caused.session_id,
        Some("stop this session"),
        Some(crate::goal::GoalStatus::Active),
        None,
    )
    .await
    .unwrap();

    let receipt = crate::session::stop_session(
        &node,
        &caused.node_did,
        &caused.session_id,
        caused.requester_did.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(receipt.status, "stop_requested");
    assert_eq!(receipt.drained_request_ids, vec![caused.request_id.clone()]);
    assert!(crate::session::session_is_stopped(
        &node,
        &caused.node_did,
        &caused.session_id,
        caused.requester_did.as_deref(),
    )
    .await
    .unwrap());
    assert_eq!(
        crate::goal::load_canonical_goal(&node, &caused.node_did, &caused.session_id)
            .await
            .unwrap()
            .unwrap()
            .parsed_status(),
        Some(crate::goal::GoalStatus::Paused)
    );

    let error = plan(
        &node,
        &SessionMessageCause {
            caller_node_did: caller.node_did.clone(),
            caller_agent_id: "caller-agent".to_owned(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: caller.request_id.clone(),
            caller_request_doc_id: caller.doc_id.clone(),
            caller_hop: caller.request_hop,
            tool_call_id: "late-message".to_owned(),
            tool_call_doc_id: "late-message-doc".to_owned(),
            correlation: None,
        },
        &SessionMessageTarget {
            node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            session_id: caused.session_id.clone(),
        },
        RenderedBody {
            content: "too late".to_owned(),
            goal: None,
        },
        None,
        false,
    )
    .await
    .unwrap();
    assert!(
        error.is_err(),
        "late message admission must honor durable stop"
    );

    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn agent_lists_separate_targets_from_messageable_sessions() {
    let (message, caller, caused) = started("agent-list").await;
    let node = message.admission.node.clone();
    let tools = AgentToolConfig {
        enabled: true,
        targets: vec![crate::document_config::AgentTargetDocument {
            target_id: "worker".to_owned(),
            node_did: caller.node_did.clone(),
            target_node_did: caused.node_did.clone(),
            agent_id: caused.agent_id.clone(),
            name: "worker".to_owned(),
            description: Some("does the work".to_owned()),
            tags: Vec::new(),
        }],
    };
    let listed = agent_list(&node, &caller, &tools).await.unwrap();
    assert_eq!(listed["agents"][0]["agent"], "worker");
    assert_eq!(listed["agents"][0]["agent_id"], caused.agent_id);
    assert!(listed.get("sessions").is_none());
    let listed = agent_session_list(&node, &caller, &tools, &AgentSessionListArgs::default())
        .await
        .unwrap();
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{listed}");
    assert_eq!(sessions[0]["session_id"], caused.session_id);
    assert_eq!(sessions[0]["relationship"], "started_by_you");
    assert_eq!(sessions[0]["can_message"], true);
    assert_eq!(sessions[0]["can_interrupt"], true);
    assert_eq!(sessions[0]["status"], "idle");

    let from_child = agent_session_list(&node, &caused, &tools, &AgentSessionListArgs::default())
        .await
        .unwrap();
    let sessions = from_child["sessions"].as_array().unwrap();
    for session in sessions {
        assert!(resolve_send_target(
            &node,
            &caused.node_did,
            &tools,
            session["session_id"].as_str().unwrap()
        )
        .await
        .unwrap()
        .is_some());
    }
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[test]
fn session_target_authority_matches_lean_owner() {
    use gents_protocol::session::{AgentSession, SessionProvenance};
    let cases = &crate::lean_vocab_test::lean_causal_hop_contract().session_target_cases;
    assert_eq!(cases.len(), 16);
    for case in cases {
        let session = AgentSession {
            session_id: "worker-session".to_owned(),
            node_did: if case.same_node { "caller" } else { "peer" }.to_owned(),
            requester_did: Some(
                if case.same_requester {
                    "caller"
                } else {
                    "other"
                }
                .to_owned(),
            ),
            agent_id: "worker".to_owned(),
            created_at: "2026-10-14T00:00:00Z".to_owned(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: Vec::new(),
            observation: None,
            provenance: case.caused.then(|| SessionProvenance {
                parent_request_doc_id: Some("parent".to_owned()),
                ..Default::default()
            }),
        };
        let tools = AgentToolConfig {
            enabled: true,
            targets: if case.allowed_target {
                vec![crate::document_config::AgentTargetDocument {
                    target_id: "worker".to_owned(),
                    node_did: "caller".to_owned(),
                    target_node_did: session.node_did.clone(),
                    agent_id: "worker".to_owned(),
                    name: "worker".to_owned(),
                    description: None,
                    tags: Vec::new(),
                }]
            } else {
                Vec::new()
            },
        };
        assert_eq!(
            session_target_allowed(&session, "caller", &tools),
            case.expected_allowed,
            "{case:?}"
        );
    }
}

#[tokio::test]
async fn session_listing_pages_by_logical_labels_and_compares_timestamp_instants() {
    let (message, caller, caused) = started("session-list-order-and-time").await;
    let node = message.admission.node.clone();
    for label in ["a-worker", "z-worker", "m-worker"] {
        let mut candidate = caused.clone();
        candidate.session_id = label.to_owned();
        candidate.caused_by_parent_request_doc_id = None;
        materialize_session_at(&node, &candidate, "2026-10-13T00:00:00Z").await;
    }
    let tools = AgentToolConfig {
        enabled: true,
        targets: Vec::new(),
    };
    let mut args: AgentSessionListArgs = serde_json::from_value(serde_json::json!({
        "limit": 1, "filter": {"relationship": "same_node"}
    }))
    .unwrap();
    let mut ordered = Vec::new();
    loop {
        let page = agent_session_list(&node, &caller, &tools, &args)
            .await
            .unwrap();
        ordered.extend(
            page["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["session_id"].as_str().unwrap().to_owned()),
        );
        args.cursor = page["next_cursor"].as_str().map(str::to_owned);
        if args.cursor.is_none() {
            break;
        }
    }
    assert_eq!(ordered, ["z-worker", "m-worker", "a-worker"]);
    for field in ["created_after", "updated_after"] {
        for (time, included) in [
            ("2026-10-12T23:59:59Z", true),
            ("2026-10-13T00:59:59+01:00", true),
            ("2026-10-13T00:00:00Z", false),
            ("2026-10-13T00:00:00+00:00", false),
            ("2026-10-13T00:00:00.001Z", false),
            ("2026-10-12T19:00:01-05:00", false),
        ] {
            let mut filter = serde_json::json!({"relationship": "same_node"});
            filter[field] = serde_json::json!(time);
            let args = serde_json::from_value(serde_json::json!({"filter": filter})).unwrap();
            let page = agent_session_list(&node, &caller, &tools, &args)
                .await
                .unwrap();
            let ids = page["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["session_id"].as_str().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                if included {
                    vec!["z-worker", "m-worker", "a-worker"]
                } else {
                    Vec::new()
                },
                "{field}: {time}"
            );
        }
    }
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn session_listing_discovers_unlinked_work_and_matches_send_authority() {
    let (message, mut caller_owner) =
        crate::tool_call_lifecycle::admission_fixture::published_session_message_with_owner(
            PublishedAdmissionOptions {
                name: "agent-session-discovery".to_owned(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let caller = caller_owner.request().clone();
    let node = message.admission.node.clone();
    let caused =
        crate::request_binding::load_agent_request_by_doc_id(&node, &message.caused_request_doc_id)
            .await
            .unwrap()
            .unwrap();
    materialize_session(&node, &caused).await;
    let tools = AgentToolConfig {
        enabled: true,
        targets: Vec::new(),
    };
    let mut candidates = Vec::new();
    for (name, owner, requester, parent) in [
        (
            "trigger-worker",
            caller.node_did.as_str(),
            caller.node_did.as_str(),
            None,
        ),
        ("other-requester", caller.node_did.as_str(), "other", None),
        ("remote-root", "peer", caller.node_did.as_str(), None),
        (
            "remote-started",
            "peer",
            caller.node_did.as_str(),
            Some(caller.doc_id.clone()),
        ),
    ] {
        let mut candidate = caused.clone();
        candidate.session_id = name.to_owned();
        candidate.node_did = owner.to_owned();
        candidate.requester_did = Some(requester.to_owned());
        candidate.caused_by_parent_request_doc_id = parent;
        materialize_session(&node, &candidate).await;
        candidates.push(candidate.session_id);
    }
    candidates.push(caused.session_id.clone());
    let mut args = AgentSessionListArgs {
        limit: Some(1),
        ..Default::default()
    };
    let mut listed = std::collections::BTreeSet::new();
    loop {
        let page = agent_session_list(&node, &caller, &tools, &args)
            .await
            .unwrap();
        for row in page["sessions"].as_array().unwrap() {
            assert_eq!(row["can_message"], true);
            assert!(
                listed.insert(row["session_id"].as_str().unwrap().to_owned()),
                "duplicate paged session"
            );
        }
        args.cursor = page["next_cursor"].as_str().map(ToOwned::to_owned);
        if args.cursor.is_none() {
            break;
        }
    }
    assert!(listed.contains("trigger-worker"));
    for id in candidates {
        assert_eq!(
            listed.contains(&id),
            resolve_send_target(&node, &caller.node_did, &tools, &id)
                .await
                .unwrap()
                .is_some(),
            "{id}"
        );
    }
    let all = agent_session_list(&node, &caller, &tools, &AgentSessionListArgs::default())
        .await
        .unwrap();
    let stale = agent_session_list(
        &node,
        &caller,
        &tools,
        &AgentSessionListArgs {
            cursor: Some("stale cursor".to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(all, stale);
    let identity =
        crate::KeyIdentity::load_or_create(message.admission.path.join("test-agent.key"), None)
            .unwrap();
    let mut worker = crate::tool_call_lifecycle::admission_fixture::claimed_signed_request_with_trigger_correlation(
        &node, "trigger-worker-request", "correlated-worker", &identity, None,
        Some("shard-assignment"), Some("R3a"),
    ).await;
    let filtered = agent_session_list(&node, &caller, &tools, &serde_json::from_value(serde_json::json!({"filter": {"caused_by_trigger_id": "shard-assignment", "caused_by_correlation": "R3a", "status": "busy"}})).unwrap()).await.unwrap();
    assert_eq!(filtered["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["sessions"][0]["session_id"], "correlated-worker");
    assert_eq!(filtered["sessions"][0]["relationship"], "same_node");
    assert_eq!(
        filtered["sessions"][0]["request_id"],
        worker.request().request_id
    );
    assert_eq!(filtered["sessions"][0]["lifecycle_state"], "claimed");
    assert_eq!(filtered["sessions"][0]["can_interrupt"], false);
    let target = resolve_send_target(&node, &caller.node_did, &tools, "correlated-worker")
        .await
        .unwrap()
        .unwrap();
    let mut send_call =
        crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
            node.clone(),
            &mut caller_owner,
            &caller.node_did,
            1,
            crate::toolset::AGENT_MESSAGE_TOOL_NAME,
            "message-trigger-worker",
            serde_json::json!({"session_id": "correlated-worker", "message": "continue shard R3a"}),
            AwaitMode::Background,
            false,
        )
        .await
        .unwrap();
    let cause = crate::lifecycle::SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_agent_id: "caller-agent".to_owned(),
        caller_session_id: "caller-session".to_owned(),
        caller_requester_did: None,
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: send_call.tool_call_id().to_owned(),
        tool_call_doc_id: send_call.doc_id().unwrap().to_owned(),
        correlation: None,
    };
    let message_plan = plan(
        &node,
        &cause,
        &target,
        RenderedBody {
            content: "continue shard R3a".to_owned(),
            goal: None,
        },
        None,
        false,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(message_plan.delivery(), Delivery::Queued);
    let delivered = commit(&node, &cause, &mut send_call, message_plan, true)
        .await
        .unwrap();
    assert_eq!(delivered.session_id, "correlated-worker");
    assert_eq!(delivered.delivery.as_deref(), Some("queue"));
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        &caller.node_did,
        std::time::Duration::ZERO,
    );
    worker.begin_owned_execution(&writer).await.unwrap();
    worker
        .terminalize_owned(
            crate::lifecycle::RequestTerminalOutcome::Interrupted,
            gents_protocol::output::TerminalOutput::NoMessage,
            None,
        )
        .await
        .unwrap();
    drop(caller_owner);
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

/// A caused session records its opening request when it is created and
/// keeps it when a later request reaches the session.
#[tokio::test]
async fn caused_session_records_its_opening_request() {
    let (message, caller, caused) = started("opening-provenance").await;
    let node = message.admission.node.clone();
    let access = ConfigAccess::local(node.clone());
    let signed = access
        .execute(&format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
            escape_graphql_string(&caused.doc_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await
        .unwrap();
    let mut signed: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(signed["data"]["AgentRequest"][0].clone()).unwrap();
    crate::request_admission::verify_request_receipt_signature(&signed).unwrap();
    signed
        .input
        .as_mut()
        .unwrap()
        .started_by
        .as_mut()
        .unwrap()
        .session_id = "forged".into();
    assert!(crate::request_admission::verify_request_receipt_signature(&signed).is_err());
    let provenance = || async {
        crate::session_origin::load_session(
            &access,
            &caused.session_id,
            caused.requester_did.as_deref(),
        )
        .await
        .unwrap()
        .unwrap()
        .provenance
        .unwrap()
    };
    let recorded = provenance().await;
    assert_eq!(
        crate::session_origin::SessionScope::from(recorded.started_by.as_ref().unwrap()),
        crate::session_origin::SessionScope::of_request(&caller)
    );
    let caller_scope = crate::session_origin::SessionScope::of_request(&caller);
    let started = crate::session_origin::lineage(&access, &caller_scope)
        .await
        .unwrap()
        .started;
    assert!(started
        .iter()
        .any(|link| link.scope == crate::session_origin::SessionScope::of_request(&caused)));
    assert_eq!(
        recorded.parent_request_doc_id,
        caused.caused_by_parent_request_doc_id
    );
    assert_eq!(
        recorded.opening_request_doc_id.as_deref(),
        Some(caused.doc_id.as_str())
    );

    let mut later = caused.clone();
    later.doc_id = "bae-later-message".to_owned();
    materialize_session(&node, &later).await;
    assert_eq!(provenance().await, recorded);

    let mut without_call = caused.clone();
    without_call.caused_by_parent_tool_call_doc_id = None;
    assert_eq!(
        crate::lifecycle::materialize::created_session_provenance(&without_call)
            .unwrap()
            .opening_request_doc_id,
        None
    );
    without_call.caused_by_parent_request_doc_id = None;
    assert!(crate::lifecycle::materialize::created_session_provenance(&without_call).is_none());
    without_call.caused_by_trigger_id = Some("task-trigger".to_owned());
    without_call.caused_by_trigger_kind = Some("event".to_owned());
    assert_eq!(
        crate::lifecycle::materialize::created_session_provenance(&without_call)
            .unwrap()
            .opening_request_doc_id
            .as_deref(),
        Some(without_call.doc_id.as_str())
    );
    materialize_session(&node, &without_call).await;
    assert_eq!(provenance().await, recorded);
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
