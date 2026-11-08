use super::*;
use crate::request::lifecycle::{
    ClaimOutcome, RequestLifecycle, RequestTerminalOutcome, TerminalizeResult,
};
use crate::tools::call::lifecycle::admission_fixture::publish_accepted_on_claimed_request;
use gents_protocol::output::TerminalOutput;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::{QueueDelivery, RequestInput};

#[tokio::test]
async fn queued_turn_preserves_canonical_file_order_across_restart() {
    check_turn_chronology(false).await;
}

#[tokio::test]
async fn retried_turn_preserves_failed_turn_file_order_across_restart() {
    check_turn_chronology(true).await;
}

async fn finish_request(
    node: &Arc<EmbeddedNode>,
    request: &mut RequestLifecycle,
    outcome: RequestTerminalOutcome,
) {
    let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
        node.clone(),
        request,
        std::time::Duration::ZERO,
    )
    .unwrap();
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            1,
            0,
            "inference.2".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        request_id: None,
        id: None,
        content: vec![gents_protocol::message::AssistantContent::Text(
            gents_protocol::message::Text {
                text: "turn ended after its edit".into(),
            },
        )],
    };
    let published = writer
        .publish_native_turn(request, 1, 0, &message)
        .await
        .unwrap();
    assert_eq!(
        request
            .terminalize_owned(
                outcome,
                TerminalOutput::Message {
                    message_doc_id: published.message_doc_id
                },
                None,
            )
            .await
            .unwrap(),
        TerminalizeResult::Won,
    );
}

async fn check_turn_chronology(retry: bool) {
    let name = if retry {
        "inventory-retry"
    } else {
        "inventory-queue"
    };
    let (fixture, mut first_request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: name.into(),
        real_identity: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let node = fixture.node;
    let mut first_tool = fixture.tool;
    grant_session_reader(
        &fixture.client,
        &fixture.node_did,
        first_tool.session_id(),
        &fixture.node_did,
    )
    .await;
    let signer = first_tool.access().unwrap().managed_agent_signer().unwrap();
    let original = first_request.request().clone();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("successor-{name}"),
        &fixture.node_did,
        fixture.requester_identity.did(),
        signer.did(),
        &original.session_id,
        &original.content,
        "interactive",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(fixture.requester_identity.did()),
    );
    let mut queued = if retry {
        None
    } else {
        create.input = crate::request::lifecycle::queue::prepare_user_message_input(
            &fixture.client,
            &fixture.node_did,
            &original.session_id,
            RequestInput::default(),
            QueueDelivery::Queue,
        )
        .await
        .unwrap();
        assert_eq!(
            create
                .input
                .queue
                .as_ref()
                .unwrap()
                .queued_after_request_id
                .as_deref(),
            Some(original.request_id.as_str()),
        );
        let mut pending =
            crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
                &node,
                &fixture.client,
                fixture.runtime.clone(),
                fixture.requester_identity.as_ref(),
                &signer,
                create.clone(),
            )
            .await
            .unwrap();
        assert_eq!(
            pending.claim_with_identity().await.unwrap(),
            ClaimOutcome::Queued
        );
        Some(pending)
    };
    first_tool.complete("first retained output").await.unwrap();
    retain_fork_fixture_capture(&node, &first_tool, "repeated.rs").await;
    finish_request(
        &node,
        &mut first_request,
        if retry {
            RequestTerminalOutcome::Failed
        } else {
            RequestTerminalOutcome::Completed
        },
    )
    .await;
    let mut successor = if let Some(pending) = queued.take() {
        pending
    } else {
        create.retry_parent_request = Some(original.request_id.clone());
        create.retry_parent_request_doc_id = Some(original.doc_id.clone());
        create.retry_root_request = Some(original.request_id.clone());
        create.retry_count = 1;
        create.input = original.input.reissued(&original.doc_id);
        crate::tools::call::lifecycle::admission_fixture::pending_client_fixture_request(
            &node,
            &fixture.client,
            fixture.runtime.clone(),
            fixture.requester_identity.as_ref(),
            &signer,
            create,
        )
        .await
        .unwrap()
    };
    assert_eq!(
        successor.claim_with_identity().await.unwrap(),
        ClaimOutcome::Claimed
    );
    if retry {
        assert_eq!(
            successor.request().retry_parent_request_doc_id.as_deref(),
            Some(original.doc_id.as_str())
        );
    }
    let mut second_tool = publish_accepted_on_claimed_request(
        node.clone(),
        &mut successor,
        &fixture.node_did,
        0,
        "write_file",
        "successor-edit",
        serde_json::json!({"path":"repeated.rs"}),
        gents_protocol::tool_call::AwaitMode::Foreground,
        true,
    )
    .await
    .unwrap();
    second_tool
        .complete("second retained output")
        .await
        .unwrap();
    retain_fork_fixture_capture(&node, &second_tool, "repeated.rs").await;
    finish_request(&node, &mut successor, RequestTerminalOutcome::Completed).await;
    let session = ConfigAccess::transact_local_readonly(
        &node,
        original
            .requester_did
            .clone()
            .map(|did| identity::Did::new(did).unwrap()),
        "test.inventory.turn_session",
        |txn| {
            let original = &original;
            Box::pin(async move {
                crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                    txn,
                    &original.node_did,
                    &original.session_id,
                    original.requester_did.as_deref(),
                )
                .await
            })
        },
    )
    .await
    .unwrap()
    .unwrap();
    let admission = FileInventoryAdmission {
        session_doc_id: session.doc_id,
        request: HydrationRequest {
            request_key: format!("turn-inventory-{name}"),
            peer_id: "fixture-peer".into(),
            requester_did: original.requester_did.clone().unwrap(),
            node_did: fixture.node_did.clone(),
            session_id: original.session_id,
        },
    };
    let contracts: serde_json::Value = tokio::task::spawn_blocking(
        gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
    )
    .await
    .unwrap()
    .unwrap();
    let physical_tools = [
        first_tool.doc_id().unwrap().to_owned(),
        second_tool.doc_id().unwrap().to_owned(),
    ];
    let expected = [
        (physical_tools[0].as_str(), 90),
        (physical_tools[1].as_str(), 2),
    ];
    let snapshot = FileInventorySnapshot::load(&node, &admission)
        .await
        .unwrap();
    assert_eq!(snapshot.entries.len(), 2);
    assert_modeled_resource_order(&contracts, "sequential_resource", &snapshot, &expected);
    assert_eq!(snapshot.binding.coverage.pending, 0);
    assert_eq!(snapshot.binding.coverage.unknown, 0);
    drop((first_request, successor, first_tool, second_tool));
    node.shutdown().await.expect("node shutdown");
    drop(node);
    drop(fixture.home);
    let reopened = crate::eval::runner::embedded::EmbeddedHome::open_retained(&fixture.path)
        .await
        .unwrap();
    assert_eq!(
        crate::store::identity::NodeIdentity::did(reopened.identity.as_ref()),
        fixture.node_did
    );
    let restored = FileInventorySnapshot::load(&reopened.node, &admission)
        .await
        .unwrap();
    assert_eq!(restored.binding, snapshot.binding);
    assert_eq!(restored.entries, snapshot.entries);
    assert_modeled_resource_order(&contracts, "sequential_resource", &restored, &expected);
    reopened.node.shutdown().await.expect("node shutdown");
    drop(reopened);
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}
