use super::*;

fn receipt() -> FileImageHydrationReceipt {
    FileImageHydrationReceipt {
        version: FILE_IMAGE_RECEIPT_VERSION,
        demand: FileImageDemand {
            inventory_reference: None,
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

fn inventory_receipt() -> FileImageHydrationReceipt {
    use crate::file_inventory::{
        FileInventoryAdmission, FileInventoryBinding, FileInventoryCoverage, FileInventoryCursor,
    };
    let mut result = receipt();
    let mut root = result.demand.admission.clone();
    root.request_key = "metadata-root".into();
    let admission = FileInventoryAdmission {
        request: root,
        session_doc_id: "physical-root".into(),
    };
    result.demand.inventory_reference = Some(FileImageInventoryReference {
        page: FileInventoryPageDemand {
            admission: admission.clone(),
            attempt_id: "metadata-attempt".into(),
            cursor: Some(FileInventoryCursor {
                binding: FileInventoryBinding {
                    admission,
                    digest: "reader-snapshot".into(),
                    coverage: FileInventoryCoverage {
                        entries: 65,
                        pending: 0,
                        unknown: 0,
                    },
                },
                offset: 64,
            }),
        },
        snapshot_digest: "reader-snapshot".into(),
        entry_index: 0,
    });
    result.version = result.demand.receipt_version();
    result
}

#[test]
fn inventory_image_keys_and_receipts_bind_the_exact_page_and_selection() {
    let original = inventory_receipt();
    original.validate_shape().unwrap();
    let key = original.demand.derive_request_key();
    let retry = original.demand.derive_retry_request_key("explicit-retry");
    let payload = original.signing_payload().unwrap();
    assert_ne!(key, receipt().demand.derive_request_key());
    assert_ne!(payload, receipt().signing_payload().unwrap());
    for path in [
        "/page/admission/session_doc_id",
        "/page/admission/request/request_key",
        "/page/admission/request/peer_id",
        "/page/admission/request/requester_did",
        "/page/admission/request/node_did",
        "/page/admission/request/session_id",
        "/page/attempt_id",
        "/page/cursor/binding/digest",
        "/page/cursor/binding/admission/session_doc_id",
        "/page/cursor/binding/coverage/entries",
        "/page/cursor/offset",
        "/snapshot_digest",
        "/entry_index",
    ] {
        let mut value = serde_json::to_value(&original.demand.inventory_reference).unwrap();
        let field = value.pointer_mut(path).unwrap();
        *field = if field.is_number() {
            serde_json::json!(1)
        } else {
            serde_json::json!("other")
        };
        let mut changed = original.clone();
        changed.demand.inventory_reference = serde_json::from_value(value).unwrap();
        assert_ne!(changed.demand.derive_request_key(), key, "{path}");
        assert_ne!(
            changed.demand.derive_retry_request_key("explicit-retry"),
            retry,
            "{path}"
        );
        assert_ne!(changed.signing_payload().unwrap(), payload, "{path}");
    }
}

#[test]
fn inventory_image_reference_refuses_cross_reader_and_snapshot_bindings() {
    let original = inventory_receipt();
    for path in [
        "/inventory_reference/page/admission/request/peer_id",
        "/inventory_reference/page/admission/request/requester_did",
        "/inventory_reference/page/admission/request/node_did",
        "/inventory_reference/page/admission/request/session_id",
        "/inventory_reference/snapshot_digest",
    ] {
        let mut value = serde_json::to_value(&original.demand).unwrap();
        *value.pointer_mut(path).unwrap() = serde_json::json!("other");
        let changed: FileImageDemand = serde_json::from_value(value).unwrap();
        assert!(changed.validate_reference().is_err(), "{path}");
    }
    let mut oversized = original.clone();
    oversized
        .demand
        .inventory_reference
        .as_mut()
        .unwrap()
        .entry_index = 64;
    assert!(oversized.validate_shape().is_err());
    let mut downgraded = original;
    downgraded.version = FILE_IMAGE_RECEIPT_VERSION;
    assert!(downgraded.validate_shape().is_err());
    let mut upgraded = receipt();
    upgraded.version = FILE_INVENTORY_IMAGE_RECEIPT_VERSION;
    assert!(upgraded.validate_shape().is_err());
}

#[test]
fn legacy_receipts_keep_their_original_payload_and_omit_inventory_binding() {
    let original = receipt();
    let encoded = serde_json::to_value(&original).unwrap();
    assert!(encoded["demand"].get("inventory_reference").is_none());
    let decoded: FileImageHydrationReceipt = serde_json::from_value(encoded).unwrap();
    decoded.validate_shape().unwrap();
    assert_eq!(
        decoded.signing_payload().unwrap(),
        original.signing_payload().unwrap()
    );
    assert_eq!(
        decoded.demand.derive_request_key(),
        original.demand.derive_request_key()
    );
    let mut explicit_null = serde_json::to_value(&original).unwrap();
    explicit_null["demand"]["inventory_reference"] = serde_json::Value::Null;
    let explicit_null: FileImageHydrationReceipt = serde_json::from_value(explicit_null).unwrap();
    assert_eq!(
        explicit_null.signing_payload().unwrap(),
        original.signing_payload().unwrap()
    );
}
