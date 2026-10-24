use super::*;
use crate::agent::p2p_reconcile::session_hydration::HydrationDocument;
use crate::agent::p2p_reconcile::session_hydration_closure::CanonicalPublicationOrigin;
use crate::session::canonical_rows::TranscriptMessageRow;
use gents_protocol::file_inventory::{FileChangeMetadata, FileImageMetadata};
use gents_protocol::output::origin::{lookup_message, resolve_origin, ObservedMessage};
use gents_protocol::output::{OutputOutcome, PayloadRef, TranscriptMessage};
use gents_protocol::session_hydration::{HydrationRequest, SessionHydrationCollection};
use serde_json::{json, Value};

// These labels bind metadata representations; source admission and capture
// integrity remain with the separate database-backed canonical owners.
fn document(value: &Value) -> String {
    format!("document-{:020}", value.as_u64().unwrap())
}

fn text(value: &Value) -> String {
    value.as_str().unwrap().to_owned()
}

fn admission(value: &Value) -> FileInventoryAdmission {
    FileInventoryAdmission {
        session_doc_id: document(&value["native_session"]),
        request: HydrationRequest {
            request_key: text(&value["key"]),
            peer_id: text(&value["peer"]),
            requester_did: text(&value["requester"]),
            node_did: text(&value["node"]),
            session_id: text(&value["session"]),
        },
    }
}

fn tool(value: &Value) -> Admission {
    Admission {
        document: document(&value["id"]),
        request: document(&value["request"]),
        scope: FileImageScope {
            node_did: text(&value["scope"]["node"]),
            session_id: text(&value["scope"]["session"]),
            requester_did: text(&value["scope"]["requester"]),
        },
        state: ToolCallState::from_persisted(value["state"].as_str().unwrap()).unwrap(),
        accepted: value["accepted"].as_bool().unwrap(),
    }
}

fn window(value: &Value) -> Window {
    Window {
        tool: document(&value["tool"]),
        admitted: value["admitted"].as_u64().unwrap().try_into().unwrap(),
        delivered: value["delivered"].as_u64().unwrap().try_into().unwrap(),
    }
}

fn ids(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()
        .map(|values| values.iter().map(document).collect())
}

fn session(value: &Value, selected: &Value) -> String {
    if value == &selected["admission"]["native_session"] {
        text(&selected["admission"]["session"])
    } else {
        format!("origin-session-{}", value.as_u64().unwrap())
    }
}

fn message(value: &Value, selected: &Value) -> TranscriptMessageRow {
    let header = &value["header"];
    let publication = &header["publication"];
    let publication = match publication["kind"].as_str().unwrap() {
        "request_execution" => MessagePublication::RequestExecution {
            execution_generation: publication["generation"].to_string(),
        },
        "request_recovery" => MessagePublication::RequestRecovery {
            execution_generation: publication["generation"].to_string(),
        },
        "tool_delivery" => MessagePublication::ToolDelivery {
            tool_call_doc_id: document(&publication["call"]),
        },
        "fork" => MessagePublication::Fork {
            origin_message_doc_id: document(&publication["origin"]),
        },
        other => panic!("unknown modeled publication {other}"),
    };
    let blocks = value["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|block| match block["kind"].as_str().unwrap() {
            "tool_call" => MessageBlock::ToolCall {
                tool_call_doc_id: document(&block["doc_id"]),
                id: text(&block["id"]),
                call_id: block["call_id"].as_str().map(str::to_owned),
                name: text(&block["name"]),
                arguments: PayloadRef {
                    close_doc_id: document(&block["arguments"]["reference"]["close_id"]),
                    stream: block["arguments"]["reference"]["stream"]
                        .as_u64()
                        .unwrap()
                        .try_into()
                        .unwrap(),
                },
                signature: block["signature"].as_str().map(str::to_owned),
                additional_params: (!block["additional_params"].is_null())
                    .then(|| block["additional_params"].clone()),
            },
            "tool_result" => MessageBlock::ToolResult {
                tool_call_doc_id: document(&block["doc_id"]),
                id: text(&block["id"]),
                call_id: block["call_id"].as_str().map(str::to_owned),
                parts: block["parts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|part| {
                        assert_eq!(part["kind"], "text");
                        let payload = &part["payload"];
                        gents_protocol::output::ToolResultPart::Text {
                            text: gents_protocol::output::PresentedPayload {
                                output: PayloadRef {
                                    close_doc_id: document(&payload["reference"]["close_id"]),
                                    stream: payload["reference"]["stream"]
                                        .as_u64()
                                        .unwrap()
                                        .try_into()
                                        .unwrap(),
                                },
                                presentation: gents_protocol::output::PayloadPresentation::Full,
                            },
                        }
                    })
                    .collect(),
            },
            other => panic!("unknown modeled block {other}"),
        })
        .collect();
    TranscriptMessageRow {
        doc_id: document(&header["id"]),
        message: TranscriptMessage {
            message_key: text(&value["key"]),
            session_id: session(&header["session"], selected),
            node_did: text(&selected["admission"]["node"]),
            requester_did: Some(text(&selected["admission"]["requester"])),
            request_doc_id: (!header["request"].is_null()).then(|| document(&header["request"])),
            publication,
            outcome: serde_json::from_value::<OutputOutcome>(header["outcome"].clone()).unwrap(),
            sequence: value["sequence"].as_u64().unwrap().try_into().unwrap(),
            role: serde_json::from_value(header["role"].clone()).unwrap(),
            native_id: value["native_id"].as_str().map(str::to_owned),
            blocks,
            created_at: value["created_at"].to_string(),
        },
    }
}

fn segment(value: &Value, selected: &Value) -> crate::session::canonical_rows::OutputSegmentRow {
    let coordinate = &value["coordinate"];
    let source = &coordinate["source"];
    let source = match source["kind"].as_str().unwrap() {
        "tool" => json!({"kind":"tool_call", "tool_call_doc_id":document(&source["call"])}),
        "authored" => json!({"kind":"authored", "key":source["key"].to_string()}),
        other => panic!("unhandled modeled source {other}"),
    };
    let flush = &value["flush"];
    let runs = flush["runs"].as_array().map(|runs| runs.iter().map(|run| {
        let declaration = &run["declaration"];
        assert!(matches!(declaration["kind"].as_str(), Some("tool_output" | "text")));
        json!({"stream":run["stream"],"bytes":run["bytes"],"declaration":{
            "block_index":declaration["block"],"part_index":declaration["part"],"payload":{"kind":declaration["kind"]}}})
    }).collect::<Vec<_>>()).unwrap_or_default();
    crate::session::canonical_rows::OutputSegmentRow {
        doc_id: document(&value["id"]),
        segment: serde_json::from_value(json!({
            "node_did":selected["admission"]["node"], "requester_did":selected["admission"]["requester"],
            "session_id":selected["admission"]["session"], "request_doc_id":document(&coordinate["request"]),
            "source":source,"writer":{"kind":"tool_execution","tool_call_doc_id":document(&value["writer"]["call"])},
            "ordinal":flush["ordinal"],"runs":runs,"payload":"","close":value["close"],"created_at":value["created_at"].to_string()
        })).unwrap(),
    }
}

fn selection(case: &Value) -> HydrationSelection {
    let selected = &case["snapshot"];
    let headers = case["closure"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| message(value, selected))
        .collect::<Vec<_>>();
    let documents = selected["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let row = headers
                .iter()
                .find(|row| row.doc_id == document(&value["id"]))
                .unwrap();
            HydrationDocument {
                collection: serde_json::from_value(value["collection"].clone()).unwrap(),
                doc_id: row.doc_id.clone(),
                node_did: row.message.node_did.clone(),
                requester_did: row.message.requester_did.clone().unwrap(),
                session_id: row.message.session_id.clone(),
            }
        })
        .collect::<std::collections::BTreeSet<_>>();
    let observed = headers
        .iter()
        .map(|row| ObservedMessage {
            doc_id: &row.doc_id,
            message: &row.message,
        })
        .collect::<Vec<_>>();
    let denied = case["closure"]["denied_headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(document)
        .collect::<Vec<_>>();
    let expected = admission(&selected["admission"]);
    let mut publications = Vec::new();
    for row in &documents {
        if row.collection != SessionHydrationCollection::AgentMessage {
            continue;
        }
        let loaded = lookup_message(
            &observed,
            &denied,
            &row.doc_id,
            &expected.request.node_did,
            Some(&expected.request.requester_did),
        );
        let origin = resolve_origin(
            &observed,
            &denied,
            &row.doc_id,
            &expected.request.node_did,
            Some(&expected.request.requester_did),
        );
        let (Ok(loaded), Ok(origin)) = (loaded, origin) else {
            publications.clear();
            break;
        };
        if loaded.message.session_id == expected.request.session_id {
            publications.push(CanonicalPublicationOrigin {
                header_doc_id: row.doc_id.clone(),
                origin_doc_id: origin.doc_id.to_owned(),
                sequence: loaded.message.sequence,
            });
        }
    }
    HydrationSelection {
        documents,
        headers,
        output_segments: case["closure"]["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| segment(value, selected))
            .collect(),
        publications,
        ..Default::default()
    }
}

fn entries(value: &Value) -> Vec<FileInventoryEntry> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| FileInventoryEntry {
            tool_call_doc_id: document(&entry["tool"]),
            request_doc_id: document(&entry["request"]),
            projection_doc_id: document(&entry["projection"]),
            observation_doc_id: document(&entry["observation"]),
            scope: FileImageScope {
                node_did: text(&entry["scope"]["node"]),
                requester_did: text(&entry["scope"]["requester"]),
                session_id: text(&entry["scope"]["session"]),
            },
            change: FileChangeMetadata {
                root: text(&entry["change"]["root"]),
                path: text(&entry["change"]["path"]),
                before: serde_json::from_value::<FileImageMetadata>(
                    entry["change"]["before"].clone(),
                )
                .unwrap(),
                after: serde_json::from_value::<FileImageMetadata>(
                    entry["change"]["after"].clone(),
                )
                .unwrap(),
            },
            order: match entry["order"]["kind"].as_str().unwrap() {
                "unresolved" => None,
                "ordered" => Some(FileInventoryOrdinal {
                    index: entry["order"]["index"].as_u64().unwrap(),
                    entries: entry["order"]["entries"].as_u64().unwrap(),
                }),
                other => panic!("unknown modeled order {other}"),
            },
        })
        .collect()
}

fn captured_entries(snapshot: &Value) -> Vec<FileInventoryEntry> {
    let mut values = Vec::new();
    for row in snapshot["tools"].as_array().unwrap() {
        if row["capture"]["kind"] != "observed" {
            continue;
        }
        let projection = &row["capture"]["projection"];
        for change in projection["changes"].as_array().unwrap() {
            values.push(json!({
                "tool": row["tool"]["id"], "request": row["tool"]["request"],
                "scope": row["tool"]["scope"], "projection": projection["id"],
                "observation": projection["observation"], "change": change,
                "order": {"kind": "unresolved"},
            }));
        }
    }
    entries(&Value::Array(values))
}

#[test]
fn generated_orders_use_canonical_publication_observations() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    for case in contracts["file_inventory_order_window_cases"]
        .as_array()
        .unwrap()
    {
        let selected = selection(case);
        let expected = admission(&case["snapshot"]["admission"]);
        let actual = admission(&case["closure"]["request"]);
        let tools = case["snapshot"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| tool(&row["tool"]))
            .collect::<Vec<_>>();
        let observed =
            Publications::new(&expected, &actual, &selected, &tools).window(&tool(&case["tool"]));
        let expected = (!case["expected"].is_null()).then(|| window(&case["expected"]));
        assert_eq!(observed, expected, "{}", case["name"]);
    }
    for case in contracts["file_inventory_order_resolution_cases"]
        .as_array()
        .unwrap()
    {
        let windows = case["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(window)
            .collect::<Vec<_>>();
        let observed =
            resolve(&windows).map(|tools| tools.into_iter().map(str::to_owned).collect::<Vec<_>>());
        assert_eq!(observed, ids(&case["expected"]), "{}", case["name"]);
    }
    for case in contracts["file_inventory_order_entry_cases"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            gents_protocol::file_inventory::coherent_inventory_entries(&entries(&case["entries"])),
            case["expected"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
    for case in contracts["file_inventory_order_comparison_cases"]
        .as_array()
        .unwrap()
    {
        let selected = selection(case);
        let expected = admission(&case["snapshot"]["admission"]);
        let actual = admission(&case["closure"]["request"]);
        let tools = case["snapshot"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| tool(&row["tool"]))
            .collect::<Vec<_>>();
        let publications = Publications::new(&expected, &actual, &selected, &tools);
        let observations = tools
            .iter()
            .map(|tool| publications.window(tool))
            .collect::<Vec<_>>();
        let mut projected = captured_entries(&case["snapshot"]);
        project(&mut projected, &observations);
        let matching = projected
            .iter()
            .filter(|entry| {
                entry.scope.node_did == case["node"].as_str().unwrap()
                    && entry.change.root == case["root"].as_str().unwrap()
                    && entry.change.path == case["path"].as_str().unwrap()
            })
            .collect::<Vec<_>>();
        let observed = if matching.is_empty() {
            None
        } else {
            matching
                .iter()
                .map(|entry| {
                    entry
                        .order
                        .as_ref()
                        .map(|order| (order.index, entry.tool_call_doc_id.clone()))
                })
                .collect::<Option<Vec<_>>>()
                .map(|mut values| {
                    values.sort_by_key(|value| value.0);
                    values.into_iter().map(|value| value.1).collect::<Vec<_>>()
                })
        };
        assert_eq!(observed, ids(&case["expected"]), "{}", case["name"]);
    }
    assert_eq!(
        contracts["file_inventory_order_window_cases"]
            .as_array()
            .unwrap()
            .len(),
        13
    );
    assert_eq!(
        contracts["file_inventory_order_resolution_cases"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    assert_eq!(
        contracts["file_inventory_order_entry_cases"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(
        contracts["file_inventory_order_comparison_cases"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}
