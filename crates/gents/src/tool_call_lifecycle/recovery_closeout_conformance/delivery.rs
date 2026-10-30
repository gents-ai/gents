use crate::store::identity::NodeIdentity;
use crate::tool_call_lifecycle::admission_fixture::{
    complete_child, published_session_message, published_session_message_with_owner,
    PublishedAdmissionOptions,
};
use crate::tool_call_lifecycle::{AwaitMode, ToolCallLifecycle};
use std::sync::Arc;

use super::fixtures::{completion_obligations, update};

#[tokio::test]
async fn caused_result_is_delivered_after_the_row_outlives_its_stored_deadline() {
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-outlives-deadline".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted session message and materialize its request");
    let admission = &message.admission;
    let node = &admission.node;
    let did = admission.node_did.clone();
    let tool_doc_id = admission.tool.doc_id().unwrap().to_owned();
    let session_id = admission.tool.session_id().to_owned();
    update(node, &tool_doc_id, r#"deadline_at: "2020-01-01T00:00:00Z""#).await;

    let restart = ToolCallLifecycle::recover_all(node, &did).await.unwrap();
    assert_eq!(restart.tool_calls_recovered, 0);
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(node, &did)
            .await
            .unwrap(),
        0
    );
    let row = |node: Arc<crate::defra_node::EmbeddedNode>| {
        let tool_doc_id = tool_doc_id.clone();
        async move {
            let response = node
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ lifecycle_state cancel_cause }} }}"#,
                    crate::graphql::escape_graphql_string(&tool_doc_id)
                ))
                .await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            response.data.unwrap()["AgentToolCall"][0].clone()
        }
    };
    assert_eq!(row(node.clone()).await["lifecycle_state"], "running");
    let (notifications, _) = completion_obligations(node, &session_id, &did).await;
    assert!(notifications.is_empty(), "{notifications:?}");

    complete_child(node, &message.caused_request_id, &did, "late caused result").await;
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(node, &did)
            .await
            .unwrap(),
        1
    );
    let settled = row(node.clone()).await;
    assert_eq!(settled["lifecycle_state"], "completed");
    assert!(settled["cancel_cause"].is_null());
    let (notifications, wakes) = completion_obligations(node, &session_id, &did).await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(notifications[0].contains("late caused result"));
    assert_eq!(wakes.len(), 1);

    assert_eq!(
        ToolCallLifecycle::recover_all(node, &did)
            .await
            .unwrap()
            .tool_calls_recovered,
        0
    );
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(node, &did)
            .await
            .unwrap(),
        0
    );
    let (notifications, _) = completion_obligations(node, &session_id, &did).await;
    assert_eq!(notifications.len(), 1, "delivered exactly once");
    message.admission.node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).expect("remove exact recovery fixture");
}

#[tokio::test]
async fn a_completion_at_the_bound_returns_at_the_callers_hop() {
    use crate::store::identity::NodeIdentity;
    let message = published_session_message(PublishedAdmissionOptions {
        name: "session-message-wake-at-bound".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        ..Default::default()
    })
    .await
    .expect("publish accepted session message and materialize its request");
    let node = message.admission.node.clone();
    let did = message.admission.node_did.clone();
    let session_id = message.admission.tool.session_id().to_owned();
    let caller_doc_id = message.admission.tool.request_doc_id().unwrap().to_owned();
    crate::documents::ensure_node(&node, &did).await.unwrap();
    let response = node
        .execute(&format!(
            r#"mutation {{ update_Node(filter: {{ node_did: {{ _eq: "{}" }} }}, input: {{ max_request_hop: 1 }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&did)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        crate::KeyIdentity::load_or_create(message.admission.path.join("test-agent.key"), None)
            .unwrap(),
    );
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        crate::agent::p2p_reconcile::enrollment_authority_channel().1,
    );
    let pending_wakes = |node: Arc<crate::defra_node::EmbeddedNode>, session: String| async move {
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }}, lifecycle_state: {{ _eq: "pending" }} }}) {{ _docID }} }}"#,
                crate::graphql::escape_graphql_string(&session)
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["_docID"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let admit = |doc_id: String| {
        let node = node.clone();
        let verifier = &verifier;
        async move {
            let request = crate::request::binding::load_agent_request_by_doc_id(&node, &doc_id)
                .await
                .unwrap()
                .unwrap();
            let behavior = request.agent_id.clone();
            let admitted = crate::agent::daemon::verify_request_at_claim_boundary(
                verifier,
                node.clone(),
                &behavior,
                request,
            )
            .await;
            let row = node
                .execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_hop lifecycle_state failure_reason }} }}"#,
                    crate::graphql::escape_graphql_string(&doc_id)
                ))
                .await
                .data
                .unwrap()["AgentRequest"][0]
                .clone();
            (admitted.is_some(), row)
        }
    };

    complete_child(
        &node,
        &message.caused_request_id,
        &did,
        "result at the bound",
    )
    .await;
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(&node, &did)
            .await
            .unwrap(),
        1
    );
    let (notifications, _) = completion_obligations(&node, &session_id, &did).await;
    assert_eq!(notifications.len(), 1, "{notifications:?}");
    assert!(notifications[0].contains("result at the bound"));
    let wakes = pending_wakes(node.clone(), session_id.clone()).await;
    assert_eq!(wakes.len(), 1);

    let caller = crate::request::binding::load_agent_request_by_doc_id(&node, &caller_doc_id)
        .await
        .unwrap()
        .unwrap();
    let native =
        crate::request::lifecycle::queue::persist_background_completion_with_message_waking(
            &node,
            &caller,
            "process done",
            "background-completion-notification:process:tool",
            crate::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
            crate::request::lifecycle::queue::RequestQueue {
                delivery: Default::default(),
                position: None,
                source: crate::request::lifecycle::queue::QueueSource::BackgroundCompletion,
                policy: crate::request::lifecycle::queue::QueuePolicy::Coalesce,
                key: Some(format!("background_completion:{session_id}")),
                queued_after_request_id: Some(caller.request_id.clone()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            },
            None,
            crate::request::lifecycle::RequestHopCause::Continuation,
        )
        .await
        .unwrap();
    assert!(!native.created_request);
    assert_eq!(pending_wakes(node.clone(), session_id.clone()).await, wakes);
    crate::test_support::install_test_agent(&node, &did, &caller.agent_id).await;
    let (admitted, row) = admit(wakes[0].clone()).await;
    assert!(admitted, "{row}");
    assert_eq!(row["request_hop"], caller.request_hop);
    assert_eq!(row["lifecycle_state"], "pending");
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}

#[tokio::test]
async fn a_paired_client_session_receives_its_agent_new_result() {
    let (message, mut calling) = published_session_message_with_owner(PublishedAdmissionOptions {
        name: "paired-client-agent-new".to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        paired_client: true,
        ..Default::default()
    })
    .await
    .expect("publish a paired client's agent_new and materialize its request");
    let node = message.admission.node.clone();
    let did = message.admission.node_did.clone();
    let session_id = message.admission.tool.session_id().to_owned();
    let caller_doc_id = message.admission.tool.request_doc_id().unwrap().to_owned();
    let caller = crate::request::binding::load_agent_request_by_doc_id(&node, &caller_doc_id)
        .await
        .unwrap()
        .unwrap();
    let client = caller
        .requester_did
        .clone()
        .expect("paired client requester");
    assert_ne!(client, did);

    complete_child(&node, &message.caused_request_id, &did, "gatekeeper answer").await;
    assert_eq!(
        crate::background_completion::settle_running_session_message_rows(&node, &did)
            .await
            .unwrap(),
        1
    );
    ToolCallLifecycle::reconcile_background_completion_side_effects(&node, &did)
        .await
        .unwrap();
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }}, execution_origin: {{ _eq: "scheduled" }} }}) {{ _docID }} }}"#,
            crate::graphql::escape_graphql_string(&session_id)
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let wakes = response.data.unwrap()["AgentRequest"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(wakes.len(), 1, "{wakes:?}");
    let wake = crate::request::binding::load_agent_request_by_doc_id(
        &node,
        wakes[0]["_docID"].as_str().unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wake.requester_did.as_deref(), Some(client.as_str()));
    assert_eq!(wake.request_hop, caller.request_hop);
    calling
        .terminalize_owned(
            crate::request::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: message
                    .admission
                    .tool
                    .accepted_header_doc_id()
                    .expect("accepted caller assistant header")
                    .to_owned(),
            },
            None,
        )
        .await
        .unwrap();

    crate::test_support::install_test_agent(&node, &did, &caller.agent_id).await;
    let identity: Arc<dyn NodeIdentity> = Arc::new(
        crate::KeyIdentity::load_or_create(message.admission.path.join("test-agent.key"), None)
            .unwrap(),
    );
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        node.clone(),
        identity,
        crate::agent::p2p_reconcile::enrollment_authority_channel().1,
    );
    let verified = crate::agent::daemon::verify_request_at_claim_boundary(
        &verifier,
        node.clone(),
        &wake.agent_id,
        wake.clone(),
    )
    .await
    .expect("the returned result is admitted into the client's session");
    let mut lifecycle = crate::RequestLifecycle::new_with_node_did(
        node.clone(),
        &wake.agent_id,
        &did,
        verified.clone(),
        60,
    );
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
        crate::request::lifecycle::ClaimOutcome::Claimed
    );
    crate::hook::DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        &session_id,
        &did,
        verified.requester_did.as_deref(),
        crate::hook::FailurePolicy::FailClosed,
    )
    .await
    .expect("the result resumes the client's session");
    node.shutdown().await;
    std::fs::remove_dir_all(&message.admission.path).unwrap();
}
