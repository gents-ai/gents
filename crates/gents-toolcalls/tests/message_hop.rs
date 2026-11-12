use gents_protocol::request_lifecycle::SessionMessageTarget;
use gents_protocol::tool_call::AwaitMode;
use gents_session::session::message::plan::PlannedWrite;
use gents_session::session::message::Delivery;
use gents_session::session::message::*;
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::call::lifecycle::admission_fixture::{
    cause, published_session_message, signer, PublishedAdmissionOptions,
};

#[tokio::test]
async fn stage_planned_write_in_txn_creates_the_request_and_leaves_the_tool_row() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-staging".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        composition: gents_session::test_support::no_lifecycle_plugins(),
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let tool = &message.admission.tool;
    let tool_doc_id = tool.doc_id().unwrap().to_owned();
    let caused = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_did: caused.agent_did.clone(),
        session_id: caused.session_id.clone(),
    };
    let cause = cause(&message, 0).await;
    let body = RenderedBody {
        content: "staged".to_owned(),
        goal: None,
    };
    let planned = plan(
        &node,
        &NoSessionMessageGoals,
        &cause,
        &target,
        body,
        None,
        false,
        &signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    let load_tool = || {
        gents_toolcalls::call::lifecycle::ToolCallLifecycle::load_by_doc_id_as(
            node.clone(),
            tool.actor().clone(),
            &tool_doc_id,
            tool.node_did(),
            tool.session_id(),
            tool.requester_did(),
        )
    };
    let before = load_tool().await.unwrap().unwrap();
    let receipt_before = load_receipt::<gents_toolcalls::call::lifecycle::ToolCallLifecycle>(
        &gents_store::config_client::ConfigAccess::local_as(node.clone(), tool.actor().clone()),
        &tool_doc_id,
        tool.node_did(),
        tool.session_id(),
        tool.requester_did(),
    )
    .await
    .unwrap();
    let (cause, planned) = (&cause, &planned);
    let enqueued = message
        .admission
        .tool
        .access()
        .unwrap()
        .transact_write("session_message.test_stage", move |txn| {
            Box::pin(async move {
                stage_planned_write_in_txn(txn, &NoSessionMessageGoals, cause, planned).await
            })
        })
        .await
        .unwrap();
    let staged = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &enqueued.doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(staged.request_id, enqueued.request_id);
    assert_eq!(staged.session_id, target.session_id);
    let after = load_tool().await.unwrap().unwrap();
    assert_eq!(after.state(), before.state());
    assert_eq!(after.is_running(), before.is_running());
    let receipt_after = load_receipt::<gents_toolcalls::call::lifecycle::ToolCallLifecycle>(
        &gents_store::config_client::ConfigAccess::local_as(node.clone(), tool.actor().clone()),
        &tool_doc_id,
        tool.node_did(),
        tool.session_id(),
        tool.requester_did(),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(&receipt_after).unwrap(),
        serde_json::to_value(&receipt_before).unwrap()
    );
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn stage_planned_write_in_txn_refuses_a_session_that_moved_past_the_planned_hop() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-stale-hop".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        composition: gents_session::test_support::no_lifecycle_plugins(),
        ..Default::default()
    })
    .await
    .unwrap();
    let node = message.admission.node.clone();
    let caused = gents_session::request::binding::load_agent_request_by_doc_id_with_access(
        &message.admission.tool.access().unwrap(),
        &message.caused_request_doc_id,
    )
    .await
    .unwrap()
    .unwrap();
    let target = SessionMessageTarget {
        node_did: caused.node_did.clone(),
        agent_did: caused.agent_did.clone(),
        session_id: caused.session_id.clone(),
    };
    let stale_cause = cause(&message, 0).await;
    let body = RenderedBody {
        content: "stale".to_owned(),
        goal: None,
    };
    let planned = plan(
        &node,
        &NoSessionMessageGoals,
        &stale_cause,
        &target,
        body,
        None,
        false,
        &signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    let ahead_cause = cause(&message, 10).await;
    let ahead = plan(
        &node,
        &NoSessionMessageGoals,
        &ahead_cause,
        &target,
        RenderedBody {
            content: "ahead".to_owned(),
            goal: None,
        },
        None,
        false,
        &signer(&message),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ahead.hop() > planned.hop());
    let (ahead_cause, ahead) = (&ahead_cause, &ahead);
    message
        .admission
        .tool
        .access()
        .unwrap()
        .transact_write("session_message.test_ahead_stage", move |txn| {
            Box::pin(async move {
                stage_planned_write_in_txn(txn, &NoSessionMessageGoals, ahead_cause, ahead).await
            })
        })
        .await
        .unwrap();
    let (stale_cause, planned) = (&stale_cause, &planned);
    let error = message
        .admission
        .tool
        .access()
        .unwrap()
        .transact_write("session_message.test_stale_stage", move |txn| {
            Box::pin(async move {
                stage_planned_write_in_txn(txn, &NoSessionMessageGoals, stale_cause, planned).await
            })
        })
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("moved to hop"),
        "unexpected error: {error:#}"
    );
    let PlannedWrite::Request { create, .. } = &planned.write else {
        panic!("a message without a Goal is a request");
    };
    let response = node
        .execute(&format!(
            r#"query {{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&create.request_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let data = response.data.unwrap();
    assert!(data["AgentRequest"].as_array().unwrap().is_empty());
    node.shutdown().await.expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn agent_creation_signs_local_directory_and_omits_remote_directory() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "agent-new-directory".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        composition: gents_session::test_support::no_lifecycle_plugins(),
        ..Default::default()
    })
    .await
    .unwrap();
    let cause = cause(&message, 0).await;
    let parent = gents_protocol::request_input::RequestInput {
        cwd: Some("/workspace/project".into()),
        ..Default::default()
    };
    for (same_node, with_goal) in [(true, false), (true, true), (false, false)] {
        let target = SessionMessageTarget {
            node_did: if same_node {
                cause.caller_node_did.clone()
            } else {
                "did:key:remote".into()
            },
            agent_did: "worker".into(),
            session_id: uuid::Uuid::new_v4().to_string(),
        };
        let create = gents_session::request::lifecycle::build_session_message_request(
            &cause,
            &target,
            "do the work",
            None,
            &uuid::Uuid::new_v4().to_string(),
            None,
            1,
            None,
            None,
            &signer(&message),
        )
        .await
        .unwrap();
        let write = if with_goal {
            PlannedWrite::Goal {
                create,
                objective: "do the work".into(),
                token_budget: None,
            }
        } else {
            PlannedWrite::Request {
                create,
                delivery: Delivery::Request,
            }
        };
        let mut plan = gents_session::session::message::plan::Plan {
            session_id: target.session_id,
            hop: 1,
            steer_head: None,
            interrupt_before_publish: false,
            write,
        };
        plan.sign_agent_creation(&cause.caller_node_did, &parent, &signer(&message))
            .await
            .unwrap();
        let create = match &plan.write {
            PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
        };
        assert_eq!(
            create.input.cwd.as_deref(),
            same_node.then_some("/workspace/project")
        );
        assert_eq!(
            create.input.session_creation,
            Some(gents_protocol::request_input::SessionCreationIntent::AgentNew)
        );
        assert!(gents_store::identity::verify_did_signature(
            &create.admission.signer_did,
            &create.signing_payload(),
            &create.admission.signature
        )
        .unwrap());
        let mut altered = create.clone();
        altered.input.cwd = Some("/workspace/other".into());
        assert!(!matches!(
            gents_store::identity::verify_did_signature(
                &altered.admission.signer_did,
                &altered.signing_payload(),
                &altered.admission.signature
            ),
            Ok(true)
        ));
    }
    message
        .admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
