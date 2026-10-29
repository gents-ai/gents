use super::*;
use crate::agent::p2p_reconcile::session_hydration::SessionReadObservation;
use gents_protocol::file_image_hydration::FileImageInventoryReference;
use gents_protocol::file_inventory::{
    FileInventoryBinding, FileInventoryCursor, FileInventoryEntry, FileInventoryOrdinal,
    FileInventoryPage, FileInventoryPageDemand,
};
use serde_json::Value;

fn text(value: &Value, field: &str) -> String {
    value[field].as_str().unwrap().to_owned()
}

fn document(value: &Value, field: &str) -> String {
    value[field].as_u64().unwrap().to_string()
}

fn rows<'a>(value: &'a Value, field: &str) -> &'a [Value] {
    value[field].as_array().unwrap()
}

fn request(value: &Value) -> super::super::super::session_hydration::HydrationRequest {
    super::super::super::session_hydration::HydrationRequest {
        request_key: text(value, "key"),
        peer_id: text(value, "peer"),
        requester_did: text(value, "requester"),
        node_did: text(value, "node"),
        session_id: text(value, "session"),
    }
}

fn admission(value: &Value) -> FileInventoryAdmission {
    FileInventoryAdmission {
        request: request(value),
        session_doc_id: document(value, "native_session"),
    }
}

fn source(value: &Value) -> SessionOwner {
    let physical = value["native_session"].as_u64().unwrap();
    SessionOwner {
        session_doc_id: (physical != 0).then(|| physical.to_string()),
        session_id: text(value, "session"),
        requester_did: text(value, "requester"),
        node_did: text(value, "node"),
    }
}

fn scope(value: &Value) -> FileImageScope {
    FileImageScope {
        requester_did: text(value, "requester"),
        node_did: text(value, "node"),
        session_id: text(value, "session"),
    }
}

fn binding(value: &Value) -> FileInventoryBinding {
    FileInventoryBinding {
        admission: admission(&value["admission"]),
        digest: text(value, "digest"),
        coverage: serde_json::from_value(value["coverage"].clone()).unwrap(),
    }
}

fn cursor(value: &Value) -> Option<FileInventoryCursor> {
    (!value.is_null()).then(|| FileInventoryCursor {
        binding: binding(&value["binding"]),
        offset: value["offset"].as_u64().unwrap(),
    })
}

fn entry(value: &Value) -> FileInventoryEntry {
    FileInventoryEntry {
        tool_call_doc_id: document(value, "tool"),
        request_doc_id: document(value, "request"),
        scope: scope(&value["scope"]),
        projection_doc_id: document(value, "projection"),
        observation_doc_id: document(value, "observation"),
        change: serde_json::from_value(value["change"].clone()).unwrap(),
        order: (value["order"]["kind"] == "ordered").then(|| FileInventoryOrdinal {
            index: value["order"]["index"].as_u64().unwrap(),
            entries: value["order"]["entries"].as_u64().unwrap(),
        }),
        request_coverage: serde_json::from_value(value["request_coverage"].clone()).unwrap(),
    }
}

#[test]
fn generated_inventory_images_keep_reader_root_snapshot_and_source_distinct() {
    let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = rows(&snapshot, "file_inventory_selected_image_cases");
    assert!(!cases.is_empty());
    for case in cases {
        let metadata = &case["metadata"];
        assert_eq!(
            case["admission"]["native_session"], metadata["admission"]["native_session"],
            "the native image root is supplied by its metadata reference"
        );
        let page_demand = FileInventoryPageDemand {
            admission: admission(&metadata["admission"]),
            attempt_id: text(metadata, "attempt"),
            cursor: cursor(&metadata["cursor"]),
        };
        let demand = FileImageDemand {
            admission: request(&case["admission"]),
            request_doc_id: text(case, "request_doc_id"),
            tool_call_doc_id: text(case, "tool_doc_id"),
            inventory_reference: Some(FileImageInventoryReference {
                page: page_demand.clone(),
                snapshot_digest: text(case, "digest"),
                entry_index: u16::try_from(case["entry_index"].as_u64().unwrap()).unwrap(),
            }),
        };
        let input = &case["catalog"];
        let catalog = HydrationCatalog {
            applied_pairing_routes: rows(input, "applied_pairing_routes")
                .iter()
                .map(|row| AppliedPairingRoute {
                    peer_id: text(row, "peer"),
                    requester_did: text(row, "requester"),
                    node_did: text(row, "node"),
                })
                .collect(),
            selected_network_id: text(input, "selected_network"),
            verified_active_memberships: rows(input, "verified_active_memberships")
                .iter()
                .map(|row| VerifiedActiveMembership {
                    network_id: text(row, "network"),
                    member_did: text(row, "member"),
                })
                .collect(),
            sessions: rows(input, "sessions").iter().map(source).collect(),
            session_reads: rows(input, "session_reads")
                .iter()
                .map(|row| SessionReadObservation {
                    peer_id: text(&row["scope"], "peer"),
                    requester_did: text(&row["scope"], "requester"),
                    session_doc_id: document(&row["scope"], "native_session"),
                    source: source(&row["source"]),
                    visible: row["visible"].as_bool().unwrap(),
                })
                .collect(),
            ..Default::default()
        };
        let frame = &case["page"];
        let wire = &frame["page"];
        let nonce = if frame["request_key"] == "" {
            String::new()
        } else if case["nonce_matches"] == true {
            page_demand.derive_request_key().unwrap()
        } else {
            "another-native-page-key".into()
        };
        let page = InventoryImagePageObservation {
            owner: text(case, "page_owner"),
            signature_verified: frame["signature_verified"].as_bool().unwrap(),
            receipt: FileInventoryPageReceipt {
                version: if frame["purpose"] == "file_inventory_v2" {
                    FILE_INVENTORY_RECEIPT_VERSION
                } else {
                    1
                },
                request_key: nonce,
                signer_did: text(frame, "signer"),
                signature: vec![0; 64],
                page: FileInventoryPage {
                    binding: binding(&wire["binding"]),
                    offset: wire["offset"].as_u64().unwrap(),
                    entries: rows(wire, "entries").iter().map(entry).collect(),
                    next: cursor(&wire["next"]),
                },
            },
        };
        let observed = &case["read"];
        let read_scope = &observed["scope"];
        let read = InventoryImageReadObservation {
            admission: FileInventoryAdmission {
                request: super::super::super::session_hydration::HydrationRequest {
                    request_key: page_demand.admission.request.request_key.clone(),
                    peer_id: text(read_scope, "peer"),
                    requester_did: text(read_scope, "requester"),
                    node_did: text(read_scope, "node"),
                    session_id: text(read_scope, "session"),
                },
                session_doc_id: document(read_scope, "native_session"),
            },
            projection_doc_id: document(observed, "projection"),
            observation_doc_id: document(observed, "observation"),
            projection_visible: observed["projection_visible"].as_bool().unwrap(),
            image_visible: observed["image_visible"].as_bool().unwrap(),
            projection_owner: text(observed, "projection_owner"),
            image_owner: text(observed, "image_owner"),
        };
        let tools: Vec<_> = rows(case, "tools")
            .iter()
            .map(|row| FileImageTool {
                doc_id: text(row, "doc_id"),
                request_doc_id: text(row, "request_doc_id"),
                scope: scope(&row["scope"]),
                accepted: row["accepted"].as_bool().unwrap(),
                terminal: row["terminal"].as_bool().unwrap(),
            })
            .collect();
        let images: Vec<_> = rows(case, "images")
            .iter()
            .map(|row| FileImageObservation {
                doc_id: text(row, "doc_id"),
                tool_call_doc_id: text(row, "tool_call_doc_id"),
                request_doc_id: text(row, "request_doc_id"),
                scope: scope(&row["scope"]),
                contents: rows(row, "contents")
                    .iter()
                    .map(|content| FileImageContent {
                        doc_id: text(content, "doc_id"),
                        tool_call_doc_id: text(content, "tool_call_doc_id"),
                        scope: scope(&content["scope"]),
                    })
                    .collect(),
            })
            .collect();
        let expected: FileImageSelection =
            serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(
            select_inventory_file_images(&demand, &catalog, &page, &read, &tools, &images),
            expected,
            "{}",
            case["name"]
        );
        let delivery = match &expected {
            FileImageSelection::Selected { document } => images
                .iter()
                .find(|image| &image.doc_id == document)
                .map(FileImageObservation::delivery_documents)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(delivery).unwrap(),
            case["delivery_documents"],
            "{}",
            case["name"]
        );
        assert_eq!(
            select_file_images(&demand, &catalog, &tools, &images),
            FileImageSelection::Rejected,
            "a bound image must not use the legacy selector: {}",
            case["name"]
        );
    }
}
