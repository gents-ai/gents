use super::*;

fn receipt() -> FileImageHydrationReceipt {
    FileImageHydrationReceipt {
        version: FILE_IMAGE_RECEIPT_VERSION,
        demand: FileImageDemand {
            admission: HydrationRequest {
                request_key: "demand-1".into(),
                peer_id: "peer-1".into(),
                requester_did: "requester-1".into(),
                node_did: "node-1".into(),
                session_id: "session-1".into(),
            },
            request_doc_id: "request-document-1".into(),
            tool_call_doc_id: "tool-document-1".into(),
        },
        selection: FileImageSelection::Selected {
            document: "image-document-1".into(),
        },
        detail: String::new(),
        processed_at: "2026-10-20T00:00:00Z".into(),
        signer_did: "node-1".into(),
        signature: vec![0; 64],
    }
}

#[test]
fn receipt_payload_binds_every_reference_and_outcome_field() {
    let original = receipt();
    original.validate_shape().unwrap();
    let value = serde_json::to_value(&original).unwrap();
    let expected = original.signing_payload().unwrap();
    for path in [
        "/version",
        "/demand/admission/request_key",
        "/demand/admission/peer_id",
        "/demand/admission/requester_did",
        "/demand/admission/node_did",
        "/demand/admission/session_id",
        "/demand/request_doc_id",
        "/demand/tool_call_doc_id",
        "/selection/document",
        "/detail",
        "/processed_at",
        "/signer_did",
    ] {
        let mut changed = value.clone();
        *changed.pointer_mut(path).unwrap() = if path == "/version" {
            serde_json::json!(FILE_IMAGE_RECEIPT_VERSION + 1)
        } else {
            serde_json::json!("changed")
        };
        let changed: FileImageHydrationReceipt = serde_json::from_value(changed).unwrap();
        assert_ne!(changed.signing_payload().unwrap(), expected, "{path}");
    }
    let mut missing = original.clone();
    missing.selection = FileImageSelection::Missing;
    missing.validate_shape().unwrap();
    assert_ne!(missing.signing_payload().unwrap(), expected);
    let mut rejected = original;
    rejected.selection = FileImageSelection::Rejected;
    rejected.detail = "Access is unavailable.".into();
    rejected.validate_shape().unwrap();
    assert_ne!(
        rejected.signing_payload().unwrap(),
        missing.signing_payload().unwrap()
    );
}

#[test]
fn malformed_receipts_are_not_valid_terminal_observations() {
    let original = receipt();
    let mut invalid = Vec::new();
    let mut changed = original.clone();
    changed.version = 1;
    invalid.push(changed);
    let mut changed = original.clone();
    changed.signer_did = "other-node".into();
    invalid.push(changed);
    let mut changed = original.clone();
    changed.signature.pop();
    invalid.push(changed);
    let mut changed = original.clone();
    changed.processed_at = "not a time".into();
    invalid.push(changed);
    let mut changed = original.clone();
    changed.demand.tool_call_doc_id.clear();
    invalid.push(changed);
    let mut changed = original.clone();
    changed.selection = FileImageSelection::Selected {
        document: String::new(),
    };
    invalid.push(changed);
    let mut changed = original.clone();
    changed.selection = FileImageSelection::Rejected;
    invalid.push(changed);
    let mut changed = original;
    changed.selection = FileImageSelection::Missing;
    changed.detail = "rejected".into();
    invalid.push(changed);
    for value in invalid {
        assert!(value.validate_shape().is_err());
    }
}

#[test]
fn request_keys_distinguish_field_boundaries_and_all_selection_dimensions() {
    let original = receipt().demand;
    let key = original.derive_request_key();
    for path in [
        "/admission/peer_id",
        "/admission/requester_did",
        "/admission/node_did",
        "/admission/session_id",
        "/request_doc_id",
        "/tool_call_doc_id",
    ] {
        let mut value = serde_json::to_value(&original).unwrap();
        *value.pointer_mut(path).unwrap() = serde_json::json!("other");
        let changed: FileImageDemand = serde_json::from_value(value).unwrap();
        assert_ne!(changed.derive_request_key(), key, "{path}");
    }
    let mut left = original.clone();
    left.admission.peer_id = "a:b".into();
    left.admission.requester_did = "c".into();
    let mut right = original;
    right.admission.peer_id = "a".into();
    right.admission.requester_did = "b:c".into();
    assert_ne!(left.derive_request_key(), right.derive_request_key());
}

#[test]
fn explicit_retry_keys_preserve_scope_and_replay_identity() {
    let original = receipt().demand;
    let retry = original.derive_retry_request_key("explicit-click");
    assert_ne!(retry, original.derive_request_key());
    assert_ne!(retry, original.derive_retry_request_key("another-click"));
    assert_eq!(retry, original.derive_retry_request_key("explicit-click"));
    for (field, _) in original.reference_fields().into_iter().skip(1) {
        let mut changed = serde_json::to_value(&original).unwrap();
        if let Some(value) = changed.get_mut(field) {
            *value = serde_json::json!("other");
        } else {
            changed["admission"][field] = serde_json::json!("other");
        }
        let changed: FileImageDemand = serde_json::from_value(changed).unwrap();
        assert_ne!(
            retry,
            changed.derive_retry_request_key("explicit-click"),
            "{field}"
        );
    }
}
