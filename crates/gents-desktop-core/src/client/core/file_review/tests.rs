use super::*;
use gents::identity::KeyIdentity;
use serde_json::json;

fn demand(node: &str) -> FileImageDemand {
    let mut demand = FileImageDemand {
        admission: HydrationRequest {
            request_key: String::new(),
            peer_id: "peer\"\\id".into(),
            requester_did: "requester".into(),
            node_did: node.into(),
            session_id: "session".into(),
        },
        request_doc_id: "physical-request".into(),
        tool_call_doc_id: "physical-tool".into(),
    };
    demand.admission.request_key = demand.derive_request_key();
    demand
}

async fn signed_row(
    identity: &KeyIdentity,
    selection: FileImageSelection,
) -> (FileImageDemand, Value) {
    let demand = demand(identity.did());
    let mut row = json!({});
    for (field, value) in demand.reference_fields() {
        row[field] = json!(value);
    }
    let mut receipt = FileImageHydrationReceipt {
        version: FILE_IMAGE_RECEIPT_VERSION,
        demand: demand.clone(),
        selection,
        detail: String::new(),
        processed_at: "2026-10-20T00:00:00Z".into(),
        signer_did: identity.did().into(),
        signature: Vec::new(),
    };
    let (status, document) = match &receipt.selection {
        FileImageSelection::Selected { document } => ("served", json!(document)),
        FileImageSelection::Missing => ("served", Value::Null),
        FileImageSelection::Rejected => {
            receipt.detail = "not authorized".into();
            ("rejected", Value::Null)
        }
    };
    receipt.signature = identity
        .sign(&receipt.signing_payload().unwrap())
        .await
        .unwrap();
    row["status"] = json!(status);
    row["observation_doc_id"] = document;
    row["status_detail"] = json!(receipt.detail);
    row["processed_at"] = json!(receipt.processed_at);
    row["outcome_signer_did"] = json!(receipt.signer_did);
    row["outcome_signature"] = json!(bs58::encode(receipt.signature).into_string());
    (demand, row)
}

#[tokio::test]
async fn file_image_review_receipts_require_exact_scope_and_valid_node_signatures() {
    let root = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(root.path().join("node.key"), None).unwrap();
    for selection in [
        FileImageSelection::Selected {
            document: "physical-image".into(),
        },
        FileImageSelection::Missing,
        FileImageSelection::Rejected,
    ] {
        let (demand, row) = signed_row(&identity, selection.clone()).await;
        assert_eq!(
            verified_receipt(&row, &demand, &identity)
                .await
                .unwrap()
                .unwrap()
                .selection,
            selection
        );
        for field in demand
            .reference_fields()
            .into_iter()
            .map(|(field, _)| field)
            .chain([
                "status_detail",
                "processed_at",
                "outcome_signer_did",
                "outcome_signature",
                "observation_doc_id",
            ])
        {
            let mut tampered = row.clone();
            tampered[field] = json!(format!("{}-other", row[field].as_str().unwrap_or_default()));
            assert!(
                verified_receipt(&tampered, &demand, &identity)
                    .await
                    .is_err(),
                "{field}"
            );
        }
    }
}

#[tokio::test]
async fn file_image_review_admission_reuses_only_the_exact_reference() {
    let root = tempfile::tempdir().unwrap();
    let node = std::sync::Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(root.path())
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let demand = demand("node");
    admit_image_demand(&node, &demand).await.unwrap();
    admit_image_demand(&node, &demand).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let response = access.execute(&demand_query(&demand)).await.unwrap();
    let row = demand_row(&response).unwrap().unwrap();
    ensure_demand(row, &demand).unwrap();
    assert_eq!(row["status"], "pending");
    let mut foreign = demand.clone();
    foreign.tool_call_doc_id.push_str("-other");
    assert!(admit_image_demand(&node, &foreign).await.is_err());
    let response = access
        .execute("{ FileImageHydrationRequest { _docID } }")
        .await
        .unwrap();
    assert_eq!(
        response["data"]["FileImageHydrationRequest"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    node.shutdown().await;
}
