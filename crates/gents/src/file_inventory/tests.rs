use super::*;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::file_changes::{FileChangeImages, FileImage, FilePermissions, ImageFailure};
use gents_protocol::file_inventory::{
    FileChangeMetadata, FileImageMetadata, FileInventoryAdmission,
};
use gents_protocol::session_hydration::HydrationRequest;
use serde_json::Value;

fn text(value: &Value) -> String {
    value.as_str().unwrap().to_owned()
}

// These labels exercise the metadata representation only. Native snapshot
// admission still binds database-created document IDs and current ACP owners.
fn document(value: &Value) -> String {
    format!("document-{:020}", value.as_u64().unwrap())
}

fn metadata(value: &Value) -> FileImageMetadata {
    match value["kind"].as_str().unwrap() {
        "missing" => FileImageMetadata::Missing {},
        "file" => FileImageMetadata::File {
            content_hash: text(&value["content_hash"]),
        },
        "unavailable" => FileImageMetadata::Unavailable {
            reason: match value["reason"].as_str().unwrap() {
                "read_failed" => ImageFailure::ReadFailed,
                "changed_during_read" => ImageFailure::ChangedDuringRead,
                "too_large" => ImageFailure::TooLarge,
                "unsupported_entry" => ImageFailure::UnsupportedEntry,
                "mutation_incomplete" => ImageFailure::MutationIncomplete,
                reason => panic!("unrecognized modeled image failure: {reason}"),
            },
        },
        kind => panic!("unrecognized modeled image metadata: {kind}"),
    }
}

#[test]
fn metadata_wire_erases_and_rejects_image_bytes() {
    let image = FileImage::File {
        content_hash: "recorded-hash".into(),
        data: "recorded image bytes".into(),
        permissions: FilePermissions {
            readonly: false,
            unix_mode: Some(0o640),
        },
        links: Some(1),
    };
    let change = FileChangeImages {
        root: "workspace".into(),
        path: "src/main.rs".into(),
        before: image.clone(),
        after: FileImage::Missing,
    };
    let projection = FileChangeMetadata::from(&change);
    assert_eq!(
        serde_json::to_value(&projection).unwrap(),
        serde_json::json!({
            "root": "workspace", "path": "src/main.rs",
            "before": {"kind": "file", "content_hash": "recorded-hash"},
            "after": {"kind": "missing"},
        })
    );
    let mut different_bytes = change;
    if let FileImage::File { data, .. } = &mut different_bytes.before {
        *data = "different retained bytes".into();
    }
    assert_eq!(FileChangeMetadata::from(&different_bytes), projection);
    let image = serde_json::json!({"kind":"file", "content_hash":"hash", "data":"retained bytes"});
    assert!(serde_json::from_value::<FileImageMetadata>(image).is_err());
}

#[tokio::test]
async fn generated_direct_inventory_uses_native_reader_and_agent_owner() {
    use crate::store::config_client::ConfigAccess;
    use crate::tools::call::lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmissionOptions,
    };
    use serde_json::json;
    let contracts: Value =
        tokio::task::spawn_blocking(gents_lean_contract::load_contract_snapshot::<Value>)
            .await
            .unwrap()
            .unwrap();
    let cases = contracts["file_inventory_direct_reader_cases"]
        .as_array()
        .unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let (fixture, _) = published_admission_with_owner(PublishedAdmissionOptions {
            name: format!("inventory-direct-{name}"),
            real_identity: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let node = fixture.node.clone();
        let mut tool = fixture.tool;
        tool.complete("retained terminal").await.unwrap();
        let key_path = fixture.path.join("reader.key");
        let reader =
            tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key_path, None))
                .await
                .unwrap()
                .unwrap();
        assert_ne!(reader.did(), tool.requester_did().unwrap());
        let reader_access = ConfigAccess::local_as(node.clone(), reader.did().parse().unwrap());
        let session = reader_access
            .transact_readonly("test.inventory.direct_session", |txn| {
                let tool = &tool;
                Box::pin(async move {
                    crate::session::load_agent_session_owner_in_txn(
                        txn,
                        tool.node_did(),
                        tool.session_id(),
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        let mut admission = FileInventoryAdmission {
            request: HydrationRequest {
                request_key: name.into(),
                peer_id: "direct-reader".into(),
                requester_did: reader.did().into(),
                node_did: tool.node_did().into(),
                session_id: tool.session_id().into(),
            },
            session_doc_id: session.doc_id,
        };
        let model_tool = &case["source"]["tools"][0];
        for projection in case["source"]["projections"].as_array().unwrap() {
            let changes = projection["changes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|change| FileChangeMetadata {
                    root: text(&change["root"]),
                    path: text(&change["path"]),
                    before: metadata(&change["before"]),
                    after: metadata(&change["after"]),
                })
                .collect::<Vec<_>>();
            let owner: ::identity::Did = if projection["owner"] == model_tool["agent_did"] {
                tool.actor().clone()
            } else {
                fixture.node_did.parse().unwrap()
            };
            let input = json!({
                "tool_call_doc_id": tool.doc_id().unwrap(),
                "request_doc_id": tool.request_doc_id().unwrap(),
                "node_did": tool.node_did(), "session_id": tool.session_id(),
                "requester_did": tool.requester_did(),
                "observation_doc_id": document(&projection["observation"]),
                "changes": changes,
            });
            let id = ConfigAccess::local_as(node.clone(), owner.clone())
                .transact_write("test.inventory.direct_projection", |txn| {
                    let input = &input;
                    Box::pin(async move {
                        let response = txn.execute_with_variables(
                            "mutation($input: ToolFileInventoryMutationInputArg!) { create_ToolFileInventory(input: $input) { _docID } }",
                            &json!({"input": input}),
                        ).await?;
                        crate::store::graphql::created_doc_id(&response, "ToolFileInventory")
                    })
                }).await.unwrap();
            node.add_dac_actor_relationship(
                owner.as_str(),
                "ToolFileInventory",
                &id,
                "reader",
                reader.did(),
            )
            .await
            .unwrap();
        }
        let denied_node = if case["source"]["catalog"]["session_reads"]
            .as_array()
            .is_some_and(|reads| reads.iter().any(|read| read["visible"] == false))
        {
            let denied =
                std::sync::Arc::new(defra_node::Node::builder().build().await.unwrap());
            let policy = denied
                .add_dac_policy_as(
                    &defra_core::RequestContext::default()
                        .with_identity(Some(fixture.node_did.clone())),
                    "name: Inventory session read\nresources:\n  - name: sessions\n    relations:\n      - name: reader\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n",
                )
                .await
                .unwrap();
            let schema = gents_protocol::schemas::AGENT_SESSION.replace(
                "type AgentSession ",
                &format!("type AgentSession @policy(id: \"{policy}\", resource: \"sessions\") "),
            );
            ConfigAccess::local(denied.clone())
                .add_collection(&schema)
                .await
                .unwrap();
            let owner = ConfigAccess::local_as(denied.clone(), tool.actor().clone());
            let input = json!({"session_id": tool.session_id(), "node_did": tool.node_did(),
                "requester_did": tool.requester_did(), "agent_did": tool.actor().as_str(),
                "created_at": "2026-10-24T00:00:00Z"});
            admission.session_doc_id = owner.transact_write("test.inventory.denied_session", |txn| {
                let input = &input;
                Box::pin(async move {
                    let result = txn.execute_with_variables(
                        "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                        &json!({"input": input})).await?;
                    crate::store::graphql::created_doc_id(&result, "AgentSession")
                })
            }).await.unwrap();
            Some(denied)
        } else {
            None
        };
        let access = if case["actor"] == "" {
            ConfigAccess::local(node.clone())
        } else if case["actor"] != case["admission"]["requester"]
            || case["source"]["boundary"] != "reader_bound"
        {
            ConfigAccess::local_as(node.clone(), fixture.node_did.parse().unwrap())
        } else {
            ConfigAccess::local_as(
                denied_node.as_ref().unwrap_or(&node).clone(),
                reader.did().parse().unwrap(),
            )
        };
        let result = FileInventorySnapshot::load_on(&access, &admission).await;
        assert_eq!(
            result.is_ok(),
            case["expected"]["kind"] == "selected",
            "{name}: {result:?}"
        );
        if let Ok(snapshot) = result {
            let page = snapshot.page(None).unwrap();
            assert_eq!(
                serde_json::to_value(&page.binding.coverage).unwrap(),
                case["page"]["binding"]["coverage"],
                "{name}"
            );
            assert_eq!(
                page.entries.len(),
                case["page"]["entries"].as_array().unwrap().len(),
                "{name}"
            );
            for entry in &page.entries {
                assert_eq!(entry.scope.requester_did, tool.requester_did().unwrap());
                assert_eq!(entry.tool_call_doc_id, tool.doc_id().unwrap());
            }
        }
        admission.session_doc_id = "wrong-physical-session".into();
        assert_eq!(
            FileInventorySnapshot::load_on(&access, &admission)
                .await
                .is_ok(),
            case["wrong_physical_expected"]["kind"] == "selected",
            "{name}"
        );
        if let Some(denied) = denied_node {
            denied.shutdown().await.expect("denied node shutdown");
        }
        node.shutdown().await.expect("node shutdown");
        tokio::fs::remove_dir_all(fixture.path).await.unwrap();
    }
}
