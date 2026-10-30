mod agent_target;
pub(in crate::agent::p2p_reconcile) mod fixtures;
mod pairing;
mod peer_admission;
mod peer_daemon;
mod replication;
mod seeding;

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::stream;
use p2p::iroh::{IrohDiscoveryConfig, IrohRelayModeConfig};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::agent::completion_retry::CompletionRetryProfileFields;
use crate::agent::daemon::{AgentDaemon, AgentDaemonInit};
use crate::agent::p2p_reconcile::templates::{
    Scope, AGENT_TARGET_CALLER_TEMPLATE, AGENT_TARGET_HOST_TEMPLATE,
};
use crate::agent::p2p_reconcile::{combine_filters, equality_filter};
use crate::agent::p2p_reconcile::{
    enrollment_authority_channel, run_enrollment_reconciler, GraphqlEnrollmentStore,
};
use crate::agent::p2p_reconcile::{resolve_template, scope_filter};
use crate::agent::runtime::StartupBarrier;
use crate::defra_node::P2PConfig;
use crate::ensure_runtime_schemas;
use crate::graphql::escape_graphql_string;
use crate::hook::{BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy};
use crate::inference::backends::provider::BackendProviderKind;
use crate::inference::llm::tool::ToolDyn;
use crate::inference::resolved::{ResolvedAgent, SamplingConfig};
use crate::prompt::LayeredPromptBuilder;
use crate::store::identity::{KeyIdentity, NodeIdentity, RuntimeNode};
use crate::tool_surface::AgentToolSurfaceConfig;
use fixtures::{
    runtime_http_test_node, runtime_test_node_with_identity, wait_for_active_peer,
    wait_for_peer_info, wait_for_value,
};
use gents_protocol::enrollment::{
    derive_enrollment_id, encode_offer, enrollment_schema_fingerprint, EnrollmentDecisionKind,
    EnrollmentOfferRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
};
use gents_protocol::network_token::NetworkRecord;

#[tokio::test]
async fn explicit_file_image_push_keeps_unselected_images_and_collection_interest_local() {
    let keys = tempfile::tempdir().unwrap();
    let sender_identity =
        KeyIdentity::load_or_create(keys.path().join("sender.key"), None).unwrap();
    let receiver_identity =
        KeyIdentity::load_or_create(keys.path().join("receiver.key"), None).unwrap();
    assert_ne!(sender_identity.did(), receiver_identity.did());
    let (sender_test, sender_http) = runtime_http_test_node(&sender_identity).await;
    let (receiver_test, receiver_http) = runtime_http_test_node(&receiver_identity).await;
    let sender = &sender_test.node;
    let receiver = &receiver_test.node;
    let sender_admin = EmbeddedRemoteP2pAdmin::new(sender.clone());
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
    sender_admin.connect(&receiver_addresses).await.unwrap();
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;
    receiver_admin
        .add_replicator(
            &sender_addresses,
            &["FileImageHydrationRequest".into()],
            &PairingFilters::default(),
        )
        .await
        .unwrap();
    let mut selected = String::new();
    for tool in ["selected-tool", "unselected-tool"] {
        let images = serde_json::json!({
            "version": 1, "tool_call_doc_id": tool, "changes": [{
                "root": "/recorded", "path": "edited.txt", "before": {"kind": "missing"},
                "after": {"kind": "unavailable", "reason": "tooLarge"}
            }]
        });
        let variables = serde_json::json!({"input": {
            "tool_call_doc_id": tool, "request_doc_id": "request",
            "node_did": sender_identity.did(), "requester_did": receiver_identity.did(),
            "session_id": "session", "file_changes": images,
        }});
        let response = crate::config_client::ConfigAccess::transact_local_write(
            sender, None, "test.file_images.seed", move |txn| {
                let variables = variables.clone();
                Box::pin(async move { txn.execute_with_variables(
                    "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                    &variables,
                ).await })
            },
        ).await.unwrap();
        if tool == "selected-tool" {
            selected = crate::graphql::created_doc_id(&response, "ToolFileChange").unwrap();
        }
    }
    let receiver_access = crate::config_client::ConfigAccess::local(receiver.clone());
    assert!(receiver_access
        .execute("{ ToolFileChange { _docID } }")
        .await
        .unwrap()["data"]["ToolFileChange"]
        .as_array()
        .unwrap()
        .is_empty());
    let peer_id = receiver.p2p().unwrap().local_peer_id().await.unwrap();
    push_documents_to_peer(
        sender,
        &peer_id,
        vec![P2pDocumentRequest {
            collection: "ToolFileChange".into(),
            doc_id: selected.clone(),
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        sender_admin
            .document_heads_acknowledged(
                &peer_id,
                "ToolFileChange",
                std::slice::from_ref(&selected)
            )
            .await
            .unwrap(),
        [false]
    );
    sender_admin
        .add_replicator(
            &receiver_addresses,
            &["FileImageHydrationRequest".into()],
            &PairingFilters::default(),
        )
        .await
        .unwrap();
    push_documents_to_peer(
        sender,
        &peer_id,
        vec![P2pDocumentRequest {
            collection: "ToolFileChange".into(),
            doc_id: selected.clone(),
        }],
    )
    .await
    .unwrap();
    assert_eq!(
        sender_admin
            .document_heads_acknowledged(
                &peer_id,
                "ToolFileChange",
                std::slice::from_ref(&selected)
            )
            .await
            .unwrap(),
        [true]
    );
    let mut demand = gents_protocol::file_image_hydration::FileImageDemand {
        inventory_reference: None,
        admission: gents_protocol::session_hydration::HydrationRequest {
            request_key: String::new(),
            peer_id: peer_id.clone(),
            requester_did: receiver_identity.did().into(),
            node_did: sender_identity.did().into(),
            session_id: "session".into(),
        },
        request_doc_id: "request".into(),
        tool_call_doc_id: "selected-tool".into(),
    };
    demand.admission.request_key = demand.derive_request_key();
    let mut receipt = gents_protocol::file_image_hydration::FileImageHydrationReceipt {
        version: gents_protocol::file_image_hydration::FILE_IMAGE_RECEIPT_VERSION,
        demand: demand.clone(),
        selection: gents_protocol::file_image_hydration::FileImageSelection::Selected {
            document: selected.clone(),
        },
        detail: String::new(),
        processed_at: "2026-10-20T00:00:00Z".into(),
        signer_did: sender_identity.did().into(),
        signature: Vec::new(),
    };
    receipt.signature = sender_identity
        .sign(&receipt.signing_payload().unwrap())
        .await
        .unwrap();
    let receipt = crate::tool_call_lifecycle::VerifiedFileImageReceipt::verify(
        receipt,
        &demand,
        &receiver_identity,
    )
    .await
    .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !receipt.grant_local_read(receiver.clone()).await.unwrap() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "image creator registration did not arrive"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    wait_for_value(
        receiver,
        "ToolFileChange",
        "tool_call_doc_id",
        "selected-tool",
    )
    .await;
    let result = receiver_access
        .execute("{ ToolFileChange { _docID tool_call_doc_id file_changes } }")
        .await
        .unwrap();
    let rows = result["data"]["ToolFileChange"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["_docID"], selected);
    assert_eq!(rows[0]["file_changes"]["tool_call_doc_id"], "selected-tool");
    let outsider_identity =
        KeyIdentity::load_or_create(keys.path().join("outsider.key"), None).unwrap();
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    for case in snapshot["stored_file_image_access_cases"]
        .as_array()
        .unwrap()
    {
        let (owner, endpoint) = match case["owner"].as_str().unwrap() {
            "node" => (sender, sender_http.as_str()),
            "client" => (receiver, receiver_http.as_str()),
            role => panic!("unknown modeled storage owner: {role}"),
        };
        let actor = case["actor"].as_str().map(|actor| match actor {
            "node" => sender_identity.did(),
            "client" => receiver_identity.did(),
            "outsider" => outsider_identity.did(),
            role => panic!("unknown modeled actor: {role}"),
        });
        let visible = if let Some(actor) = actor {
            let did = actor.parse().unwrap();
            let result = crate::config_client::ConfigAccess::transact_local_write(
                owner,
                Some(did),
                "test.file_images.storage_access",
                |txn| Box::pin(async move { txn.execute("{ ToolFileChange { _docID } }").await }),
            )
            .await
            .unwrap();
            result["data"]["ToolFileChange"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["_docID"] == selected)
        } else {
            let collection = owner.get_collection("ToolFileChange").unwrap().unwrap();
            owner
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Anonymous,
                    defra_node::DocumentPermission::Read,
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                    &collection.collection_id,
                )
                .await
                .unwrap()
        };
        assert_eq!(visible, case["expected"].as_bool().unwrap(), "{case}");
        let http = match actor {
            Some(did) => crate::config_client::ConfigAccess::graphql_as(endpoint, did),
            None => crate::config_client::ConfigAccess::graphql(endpoint),
        };
        let response = http
            .execute("{ ToolFileChange { _docID file_changes } }")
            .await
            .unwrap();
        let http_visible = response["data"]["ToolFileChange"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["_docID"] == selected);
        assert_eq!(
            http_visible,
            case["expected"].as_bool().unwrap(),
            "HTTP: {case}"
        );
    }
    let outsider_test = runtime_test_node_with_identity(&outsider_identity).await;
    let outsider = &outsider_test.node;
    let outsider_admin = EmbeddedRemoteP2pAdmin::new(outsider.clone());
    outsider_admin.connect(&receiver_addresses).await.unwrap();
    wait_for_active_peer(&outsider_admin).await;
    let pull = outsider
        .p2p()
        .unwrap()
        .sync_documents(
            "ToolFileChange",
            vec![selected.clone()],
            Some(Duration::from_secs(5)),
        )
        .await;
    let copied = crate::config_client::ConfigAccess::local(outsider.clone())
        .execute("{ ToolFileChange { _docID } }")
        .await
        .unwrap();
    assert!(
        copied["data"]["ToolFileChange"]
            .as_array()
            .unwrap()
            .is_empty(),
        "an unrelated transport peer fetched a retained image from its recipient: {pull:?}"
    );
    assert!(sender_admin
        .list_p2p_collections()
        .await
        .unwrap()
        .is_empty());
    assert!(receiver_admin
        .list_p2p_collections()
        .await
        .unwrap()
        .is_empty());
    sender.shutdown().await;
    receiver.shutdown().await;
    outsider.shutdown().await;
}
