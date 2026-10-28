use super::*;
use crate::identity::KeyIdentity;
use gents_protocol::file_image_hydration::FILE_IMAGE_RECEIPT_VERSION;
use gents_protocol::session_hydration::HydrationRequest;

#[tokio::test]
async fn file_image_read_grant_matches_signed_receipt_contract() {
    let directory = tempfile::tempdir().unwrap();
    let source = KeyIdentity::load_or_create(directory.path().join("source.key"), None).unwrap();
    let client = KeyIdentity::load_or_create(directory.path().join("client.key"), None).unwrap();
    let other = KeyIdentity::load_or_create(directory.path().join("other.key"), None).unwrap();
    let mut nodes = Vec::new();
    for (name, identity) in [("client", &client), ("other", &other)] {
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(directory.path().join(name))
                .with_node_identity_did(identity.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        nodes.push(node);
    }
    let actor = |role: &str| match role {
        "node" => source.did(),
        "client" => client.did(),
        "other" => other.did(),
        "" => "",
        role => panic!("unknown modeled actor: {role}"),
    };
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    for case in snapshot["file_image_read_grant_cases"].as_array().unwrap() {
        let node = nodes[usize::from(case["local_actor"] == "other")].clone();
        let text = |field: &str| actor(case[field].as_str().unwrap());
        let document = format!("bae-{}", uuid::Uuid::new_v4());
        let creator = text("stored_creator");
        if !creator.is_empty() {
            node.document_acp()
                .register_doc_object(
                    &creator.parse().unwrap(),
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID,
                    gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE,
                    &document,
                )
                .await
                .unwrap();
        }
        let mut demand = FileImageDemand {
            admission: HydrationRequest {
                request_key: String::new(),
                peer_id: "peer".into(),
                requester_did: text("requester").into(),
                node_did: text("source").into(),
                session_id: "session".into(),
            },
            request_doc_id: "request".into(),
            tool_call_doc_id: "tool".into(),
        };
        demand.admission.request_key = demand.derive_request_key();
        let mut receipt = FileImageHydrationReceipt {
            version: FILE_IMAGE_RECEIPT_VERSION,
            demand: demand.clone(),
            selection: FileImageSelection::Selected {
                document: document.clone(),
            },
            detail: String::new(),
            processed_at: "2026-10-20T00:00:00Z".into(),
            signer_did: text("signer").into(),
            signature: Vec::new(),
        };
        let signer = if case["signer"] == "node" {
            &source
        } else {
            &other
        };
        receipt.signature = signer
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        if !case["verified"].as_bool().unwrap() {
            receipt.signature[0] ^= 1;
        }
        let verified = VerifiedFileImageReceipt::verify(receipt, &demand, &client).await;
        let granted = match verified {
            Ok(receipt) => receipt
                .grant_observation_read(node.clone())
                .await
                .unwrap_or(false),
            Err(_) => false,
        };
        let expected = case["expected"].as_bool().unwrap();
        assert_eq!(granted, expected, "{case}");
        if !creator.is_empty() {
            let identity = defra_node::AcpIdentity::Authenticated(client.did().parse().unwrap());
            let acp = node.document_acp();
            let policy = gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_ID;
            let resource = gents_protocol::schemas::TOOL_FILE_CHANGE_POLICY_RESOURCE;
            assert_eq!(
                acp.check_doc_access(
                    &identity,
                    defra_node::DocumentPermission::Read,
                    policy,
                    resource,
                    &document
                )
                .await
                .unwrap(),
                expected,
                "{case}"
            );
            for permission in [
                defra_node::DocumentPermission::Update,
                defra_node::DocumentPermission::Delete,
            ] {
                assert!(
                    !acp.check_doc_access(&identity, permission, policy, resource, &document)
                        .await
                        .unwrap(),
                    "{case}"
                );
            }
        }
    }
    for node in nodes {
        node.shutdown().await;
    }
}
