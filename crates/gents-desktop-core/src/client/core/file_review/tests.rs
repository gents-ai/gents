use super::*;
use gents::identity::KeyIdentity;
use serde_json::json;

fn demand(node: &str) -> FileImageDemand {
    let mut demand = FileImageDemand {
        inventory_reference: None,
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
    let row = signed_demand_row(identity, &demand, selection).await;
    (demand, row)
}

async fn signed_demand_row(
    identity: &KeyIdentity,
    demand: &FileImageDemand,
    selection: FileImageSelection,
) -> Value {
    let mut row = json!({});
    for (field, value) in demand.reference_fields() {
        row[field] = json!(value);
    }
    if let Some(reference) = &demand.inventory_reference {
        row["inventory_reference"] = serde_json::to_value(reference).unwrap();
    }
    let mut receipt = FileImageHydrationReceipt {
        version: demand.receipt_version(),
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
    row
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
                .selection(),
            &selection
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
    let identity = KeyIdentity::load_or_create(root.path().join("client.key"), None).unwrap();
    let node = std::sync::Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(root.path())
            .with_node_identity_did(identity.did())
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

#[tokio::test]
async fn explicit_file_review_retry_preserves_refusals_and_resumes_verified_delivery() {
    let root = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(root.path().join("node.key"), None).unwrap();
    let node = std::sync::Arc::new(
        defra_node::EmbeddedNode::builder()
            .data_path(root.path().join("db"))
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let access = ConfigAccess::local(node.clone());
    let original = demand(identity.did());
    admit_image_demand(&node, &original).await.unwrap();
    let refused = signed_demand_row(&identity, &original, FileImageSelection::Rejected).await;
    write_receipt(&access, &original, &refused).await;
    assert_eq!(
        resumable_image_demand(&access, &original, &identity)
            .await
            .unwrap(),
        original
    );

    let mut retry = original.clone();
    retry.admission.request_key = original.derive_retry_request_key("explicit-click");
    for _ in 0..2 {
        admit_image_demand(&node, &retry).await.unwrap();
    }
    assert_eq!(
        resumable_image_demand(&access, &original, &identity)
            .await
            .unwrap(),
        retry
    );
    let selected = FileImageSelection::Selected {
        document: "physical-image".into(),
    };
    let served = signed_demand_row(&identity, &retry, selected.clone()).await;
    write_receipt(&access, &retry, &served).await;
    let mut later_pending = original.clone();
    later_pending.admission.request_key = original.derive_retry_request_key("other-click");
    admit_image_demand(&node, &later_pending).await.unwrap();
    assert_eq!(
        resumable_image_demand(&access, &original, &identity)
            .await
            .unwrap(),
        retry
    );
    let response = access.execute(&demand_query(&original)).await.unwrap();
    assert_eq!(
        verified_receipt(
            demand_row(&response).unwrap().unwrap(),
            &original,
            &identity
        )
        .await
        .unwrap()
        .unwrap()
        .selection(),
        &FileImageSelection::Rejected
    );

    let mut foreign = original.clone();
    foreign.tool_call_doc_id = "another-tool".into();
    foreign.admission.request_key = foreign.derive_request_key();
    assert_eq!(
        resumable_image_demand(&access, &foreign, &identity)
            .await
            .unwrap(),
        foreign
    );
    let mut tampered = served;
    tampered["outcome_signature"] = json!(bs58::encode([0u8; 64]).into_string());
    write_receipt(&access, &retry, &tampered).await;
    assert!(resumable_image_demand(&access, &original, &identity)
        .await
        .is_err());
    node.shutdown().await;
}

async fn write_receipt(access: &ConfigAccess, demand: &FileImageDemand, row: &Value) {
    let response = access.execute(&demand_query(demand)).await.unwrap();
    let document = demand_row(&response).unwrap().unwrap()["_docID"]
        .as_str()
        .unwrap();
    let mut input = row.clone();
    for (field, _) in demand.reference_fields() {
        input.as_object_mut().unwrap().remove(field);
    }
    let variables = json!({"document": document, "input": input});
    access.transact_write("test.file_review.receipt", |txn| {
        let variables = &variables;
        Box::pin(async move {
            txn.execute_with_variables(
                "mutation($document: ID!, $input: FileImageHydrationRequestMutationInputArg!) { update_FileImageHydrationRequest(docID: $document, input: $input) { _docID } }",
                variables,
            ).await
        })
    }).await.unwrap();
}

#[tokio::test]
async fn inactive_file_review_preserves_attempts_without_new_admission() {
    let root = tempfile::tempdir().unwrap();
    let core = std::sync::Arc::new(
        ClientCore::start_with_paths_and_options(
            crate::client::DesktopPaths::from_root(root.path().join("client")),
            crate::client::ClientCoreOptions::local_only(),
        )
        .await
        .unwrap(),
    );
    let initial = demand("unconfigured-runtime");
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        core.remote_file_images(initial),
    )
    .await
    .expect("inactive route must fail before the delivery timeout")
    .unwrap_err();
    assert_inactive_file_review_error(&error);
    let response = ConfigAccess::local(core.node_arc())
        .execute("{ FileImageHydrationRequest { _docID } }")
        .await
        .unwrap();
    assert_eq!(response["data"]["FileImageHydrationRequest"], json!([]));
    let pending = demand("unconfigured-runtime");
    admit_image_demand(&core.node_arc(), &pending)
        .await
        .unwrap();
    let access = ConfigAccess::local(core.node_arc());
    let before = access.execute(&demand_query(&pending)).await.unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        core.remote_file_images(pending.clone()),
    )
    .await
    .expect("inactive pending delivery must fail promptly")
    .unwrap_err();
    assert_inactive_file_review_error(&error);
    let after = access.execute(&demand_query(&pending)).await.unwrap();
    assert_eq!(before, after);

    let signer = KeyIdentity::load_or_create(root.path().join("source.key"), None).unwrap();
    let mut refused = demand(signer.did());
    refused.admission.requester_did = core.node_identity().did().into();
    refused.admission.request_key = refused.derive_request_key();
    admit_image_demand(&core.node_arc(), &refused)
        .await
        .unwrap();
    let receipt = signed_demand_row(&signer, &refused, FileImageSelection::Rejected).await;
    write_receipt(&access, &refused, &receipt).await;
    let before = access.execute(&demand_query(&refused)).await.unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        core.remote_file_images(refused.clone()),
    )
    .await
    .expect("a signed refusal must remain observable without an active route")
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Runtime rejected file review: not authorized"
    );
    let after = access.execute(&demand_query(&refused)).await.unwrap();
    assert_eq!(before, after);

    let mut selected = refused.clone();
    selected.admission.request_key =
        selected.derive_retry_request_key("8ff1b22a-9d76-4c48-a3c3-0e177b5423cf");
    admit_image_demand(&core.node_arc(), &selected)
        .await
        .unwrap();
    let receipt = signed_demand_row(
        &signer,
        &selected,
        FileImageSelection::Selected {
            document: "undelivered-image".into(),
        },
    )
    .await;
    write_receipt(&access, &selected, &receipt).await;
    let before = access.execute(&demand_query(&selected)).await.unwrap();
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        core.remote_file_images(selected.clone()),
    )
    .await
    .expect("an undelivered selected image must not wait on an inactive route")
    .unwrap_err();
    assert_inactive_file_review_error(&error);
    let after = access.execute(&demand_query(&selected)).await.unwrap();
    assert_eq!(before, after);
    core.shutdown().await.unwrap();
}

fn assert_inactive_file_review_error(error: &anyhow::Error) {
    let classified = error.downcast_ref::<BridgeError>().unwrap();
    assert_eq!(classified.code, BridgeErrorCode::EndpointUnreachable);
    assert!(classified.retryable);
    assert_eq!(
        classified.message,
        "Reconnect this runtime to load new file versions."
    );
}

#[tokio::test]
async fn inventory_bound_admission_and_signed_receipt_keep_structured_reference() {
    let root = tempfile::tempdir().unwrap();
    let identity = KeyIdentity::load_or_create(root.path().join("node.key"), None).unwrap();
    let node = std::sync::Arc::new(
        defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let mut demand = demand(identity.did());
    demand.inventory_reference = Some(FileImageInventoryReference {
        page: gents_protocol::file_inventory::FileInventoryPageDemand {
            admission: gents_protocol::file_inventory::FileInventoryAdmission {
                request: demand.admission.clone(),
                session_doc_id: "physical-root".into(),
            },
            attempt_id: "metadata-attempt".into(),
            cursor: None,
        },
        snapshot_digest: "metadata-snapshot".into(),
        entry_index: 0,
    });
    demand.admission.request_key = demand.derive_request_key();
    admit_image_demand(&node, &demand).await.unwrap();
    admit_image_demand(&node, &demand).await.unwrap();
    let stored = ConfigAccess::local(node.clone())
        .execute(&demand_query(&demand))
        .await
        .unwrap();
    let row = demand_row(&stored).unwrap().unwrap();
    assert!(row["inventory_reference"].is_object());
    ensure_demand(row, &demand).unwrap();
    let signed = signed_demand_row(
        &identity,
        &demand,
        FileImageSelection::Selected {
            document: "physical-image".into(),
        },
    )
    .await;
    assert!(verified_receipt(&signed, &demand, &identity)
        .await
        .unwrap()
        .is_some());
    let mut wrong = signed.clone();
    wrong["inventory_reference"]["snapshot_digest"] = json!("different-snapshot");
    assert!(verified_receipt(&wrong, &demand, &identity).await.is_err());
    node.shutdown().await;
}
