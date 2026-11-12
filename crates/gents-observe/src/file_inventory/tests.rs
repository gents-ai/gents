use super::*;
use gents_protocol::file_changes::{FileChangeImages, FileImage, FilePermissions, ImageFailure};
use gents_protocol::file_inventory::FileInventoryRequest;
use gents_protocol::file_inventory::{
    FileChangeMetadata, FileImageMetadata, FileInventoryAdmission,
};
use base64::Engine as _;
use gents_loop::live_output::LiveToolOutputRegistry;
use gents_loop::tool::Tool;
use gents_session::file_contents::content_hash;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_toolcalls::call::lifecycle::admission_fixture::{
    published_admission, published_admission_with_owner, PublishedAdmissionOptions,
};
use gents_tools::native::args::WriteFileArgs;
use gents_tools::native::file_tools::write_file::WriteFileTool;
use gents_tools::native::shared::ToolContext;
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
    use gents_store::config_client::ConfigAccess;
    use gents_toolcalls::call::lifecycle::admission_fixture::{
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
            composition: gents_automation::lifecycle_hooks::composition(),
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
        let reader_did: identity::Did = reader.did().parse().unwrap();
        let admin = ConfigAccess::local_as(node.clone(), fixture.node_did.parse().unwrap());
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&reader_did, permission)
                .await
                .unwrap();
        }
        let user_did = identity::Did::new(fixture.client.actor_did().unwrap()).unwrap();
        admin
            .grant_node_permission(
                &user_did,
                gents_store::config_client::NodePermission::DacRelationDelete,
            )
            .await
            .unwrap();
        let session = fixture
            .client
            .transact_readonly("test.inventory.direct_session", |txn| {
                let tool = &tool;
                Box::pin(async move {
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
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
        fixture
            .client
            .add_actor_relationship("AgentSession", &session.doc_id, "reader", &reader_did)
            .await
            .unwrap();
        let reader_access = ConfigAccess::local_as(node.clone(), reader_did.clone());
        let mut admission = FileInventoryAdmission {
            request: FileInventoryRequest {
                request_key: name.into(),
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
            let projection_access = if projection["owner"] == model_tool["agent_did"] {
                tool.access().unwrap()
            } else {
                admin.clone()
            };
            let input = json!({
                "tool_call_doc_id": tool.doc_id().unwrap(),
                "request_doc_id": tool.request_doc_id().unwrap(),
                "node_did": tool.node_did(), "session_id": tool.session_id(),
                "requester_did": tool.requester_did(),
                "observation_doc_id": document(&projection["observation"]),
                "changes": changes,
            });
            let id = projection_access
                .transact_write("test.inventory.direct_projection", |txn| {
                    let input = &input;
                    Box::pin(async move {
                        let response = txn.execute_with_variables(
                            "mutation($input: ToolFileInventoryMutationInputArg!) { create_ToolFileInventory(input: $input) { _docID } }",
                            &json!({"input": input}),
                        ).await?;
                        gents_store::graphql::created_doc_id(&response, "ToolFileInventory")
                    })
                }).await.unwrap();
            for subject in [&reader_did, &user_did] {
                projection_access
                    .add_actor_relationship("ToolFileInventory", &id, "reader", subject)
                    .await
                    .unwrap();
            }
        }
        let denies_session = case["source"]["catalog"]["session_reads"]
            .as_array()
            .is_some_and(|reads| reads.iter().any(|read| read["visible"] == false));
        if denies_session {
            fixture
                .client
                .delete_actor_relationship(
                    "AgentSession",
                    &admission.session_doc_id,
                    "reader",
                    &reader_did,
                )
                .await
                .unwrap();
        }
        let access = if case["actor"] == "" {
            ConfigAccess::local(node.clone())
        } else if case["actor"] != case["admission"]["requester"]
            || case["source"]["boundary"] != "reader_bound"
        {
            ConfigAccess::local_as(node.clone(), fixture.node_did.parse().unwrap())
        } else {
            reader_access
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
        let original_session_doc_id = admission.session_doc_id.clone();
        admission.session_doc_id = "wrong-physical-session".into();
        assert_eq!(
            FileInventorySnapshot::load_on(&access, &admission)
                .await
                .is_ok(),
            case["wrong_physical_expected"]["kind"] == "selected",
            "{name}"
        );
        if case["expected"]["kind"] == "selected" {
            let direct = gents_protocol::file_inventory::DirectFileInventoryRequest {
                node_did: tool.node_did().to_owned(),
                session_id: tool.session_id().to_owned(),
                action: Default::default(),
            };
            assert!(super::read_direct_inventory(&access, &direct)
                .await
                .unwrap()
                .is_some());
            let user_access = ConfigAccess::local_as(node.clone(), user_did.clone());
            assert!(super::read_direct_inventory(&user_access, &direct)
                .await
                .unwrap()
                .is_some());
            fixture
                .client
                .delete_actor_relationship(
                    "AgentSession",
                    &original_session_doc_id,
                    "reader",
                    &reader_did,
                )
                .await
                .unwrap();
            let after = super::read_direct_inventory(&access, &direct).await;
            assert!(
                matches!(after, Ok(None) | Err(_)),
                "revocation must hide inventory metadata"
            );
            assert!(
                super::read_direct_inventory(&user_access, &direct)
                    .await
                    .unwrap()
                    .is_some(),
                "revoking a shared reader must not revoke the User owner"
            );
        }
        node.shutdown().await.expect("node shutdown");
        tokio::fs::remove_dir_all(fixture.path).await.unwrap();
    }
}

fn decoded_image(value: &Value) -> Vec<u8> {
    assert_eq!(value["kind"], "file");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(value["content_hash"], content_hash(&bytes));
    bytes
}

fn assert_stored_versions(stored: &Value) {
    assert_eq!(
        stored["version"],
        gents_protocol::file_changes::STORED_FILE_CHANGES_VERSION
    );
    for change in stored["changes"].as_array().unwrap() {
        for side in ["before", "after"] {
            assert!(change[side].get("data").is_none(), "{change}");
            if change[side]["kind"] == "file" {
                assert!(change["content_doc_id"].is_string(), "{change}");
                assert!(change[side]["version"].is_string(), "{change}");
            }
        }
    }
}

#[tokio::test]
async fn plugin_file_effects_retain_exact_images_without_borrowing_parent_order() {
    let contracts: serde_json::Value = tokio::task::spawn_blocking(
        gents_lean_contract::load_contract_snapshot::<serde_json::Value>,
    )
    .await
    .unwrap()
    .unwrap();
    let modeled = contracts["file_inventory_order_window_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "accepted_effect_without_own_publication")
        .unwrap();
    assert!(modeled["expected"].is_null());
    let (fixture, _request) = published_admission_with_owner(PublishedAdmissionOptions {
        name: "inventory-plugin-effects".into(),
        real_identity: true,
        paired_client: true,
        tool_name: Some("plugin".into()),
        composition: gents_automation::lifecycle_hooks::composition(),
        ..Default::default()
    })
    .await
    .unwrap();
    fixture.grant_agent_collection_get().await.unwrap();
    fixture.grant_runtime_session_reader().await.unwrap();
    let mut parent = fixture.tool;
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("file.txt");
    tokio::fs::write(&path, "before\n").await.unwrap();
    let writer_tool = WriteFileTool::new(ToolContext::new(root_path.clone(), false).unwrap());
    let registry = LiveToolOutputRegistry::default();
    let access = parent.access().unwrap();
    let mut captures = Vec::new();
    let mut tools = Vec::new();
    for (ordinal, before, after) in [
        (1, "before\n", "first effect\n"),
        (2, "first effect\n", "second effect\n"),
    ] {
        let arguments = serde_json::json!({"path": "file.txt", "content": after,
            "overwrite": true, "raw_json": true})
        .to_string();
        let mut effect = parent
            .admit_plugin_effect(ordinal, "write_file", &arguments)
            .await
            .unwrap();
        effect.start_running().await.unwrap();
        let document = effect.doc_id().unwrap().to_owned();
        let writer = registry.writer_for(document.clone()).await;
        let output = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            None, tokio_util::sync::CancellationToken::new(), None, Some(writer), None, None,
            Default::default(), false,
            writer_tool.call(WriteFileArgs {path: "file.txt".into(), content: after.into(),
                expected_content_hash: None, overwrite: true, raw_json: true}),
        ).await.unwrap();
        effect.stage_deferred_writes(registry.deferred_writes(&document).await);
        effect.complete(&output).await.unwrap();
        let replayed = parent
            .admit_plugin_effect(ordinal, "write_file", &arguments)
            .await
            .unwrap();
        assert_eq!(replayed.doc_id(), Some(document.as_str()));
        let capture = gents_session::tool_call::query::result::load_tool_call_file_changes(
            &access,
            &document,
            effect.request_doc_id().unwrap(),
            effect.node_did(),
            effect.session_id(),
            effect.requester_did(),
        )
        .await
        .unwrap()
        .expect("terminal effect retains its native file observation");
        assert_eq!(capture.changes.len(), 1);
        let change = &capture.changes[0];
        assert_eq!(change.root, root_path.to_str().unwrap());
        assert_eq!(change.path, "file.txt");
        for (image, expected) in [(&change.before, before), (&change.after, after)] {
            let FileImage::File {
                data,
                content_hash: hash,
                ..
            } = image
            else {
                panic!("expected retained file bytes");
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .unwrap();
            assert_eq!(bytes, expected.as_bytes());
            assert_eq!(hash, &content_hash(&bytes));
        }
        captures.push(capture);
        tools.push(document);
    }
    assert_eq!(captures[0].changes[0].after, captures[1].changes[0].before);
    parent.complete("both effects completed").await.unwrap();
    let admission = ConfigAccess::transact_local_readonly(
        &fixture.node,
        Some(parent.actor().clone()),
        "test.inventory.plugin_session",
        |txn| {
            let parent = &parent;
            Box::pin(async move {
                let session =
                    gents_store::config_client::agent_session::load_agent_session_row_in_txn(
                        txn,
                        parent.node_did(),
                        parent.session_id(),
                        parent.requester_did(),
                    )
                    .await?
                    .unwrap();
                Ok::<_, anyhow::Error>(gents_protocol::file_inventory::FileInventoryAdmission {
                    session_doc_id: session.doc_id,
                    request: gents_protocol::file_inventory::FileInventoryRequest {
                        request_key: "plugin-inventory".into(),
                        node_did: parent.node_did().into(),
                        session_id: parent.session_id().into(),
                        requester_did: parent.requester_did().unwrap().into(),
                    },
                })
            })
        },
    )
    .await
    .unwrap();
    gents_toolcalls::call::lifecycle::admission_fixture::grant_runtime_projection_readers(
        &access,
        parent.node_did(),
        parent.session_id(),
    )
    .await
    .unwrap();
    let snapshot =
        crate::file_inventory::FileInventorySnapshot::load(&fixture.node, &admission)
            .await
            .unwrap();
    assert_eq!(
        snapshot.entries().len(),
        2,
        "replay cannot duplicate an effect capture"
    );
    for entry in snapshot.entries() {
        assert!(tools.contains(&entry.tool_call_doc_id));
        assert_eq!(entry.request_doc_id, parent.request_doc_id().unwrap());
        assert_eq!(entry.scope.node_did, parent.node_did());
        assert_eq!(entry.scope.session_id, parent.session_id());
        assert_eq!(entry.scope.requester_did, parent.requester_did().unwrap());
        assert_eq!(
            serde_json::to_value(&entry.order).unwrap(),
            modeled["expected"],
            "continuous images and effect ordinals do not supply a canonical publication pair"
        );
    }
    let page = snapshot.page(None).unwrap();
    assert_eq!(page.entries, snapshot.entries());
    assert!(page.next.is_none());
    let repeated =
        crate::file_inventory::FileInventorySnapshot::load(&fixture.node, &admission)
            .await
            .unwrap();
    assert_eq!(snapshot.binding(), repeated.binding());
    assert_eq!(snapshot.entries(), repeated.entries());
    assert_eq!(tokio::fs::read(&path).await.unwrap(), b"second effect\n");
    fixture.node.shutdown().await.expect("shut down test node");
    tokio::fs::remove_dir_all(fixture.path).await.unwrap();
}

#[tokio::test]
async fn native_images_commit_with_their_physical_terminal_and_stay_out_of_tool_output() {
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-change-native-terminal".into(),
        real_identity: true,
        tool_name: Some("write_file".into()),
        paired_client: true,
        composition: gents_automation::lifecycle_hooks::composition(),
        ..Default::default()
    })
    .await
    .unwrap();
    admission.grant_agent_collection_get().await.unwrap();
    admission.grant_runtime_session_reader().await.unwrap();
    let mut call = admission.tool;
    let doc_id = call.doc_id().unwrap().to_owned();
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("file.bin");
    std::fs::write(&path, [0, 255, 128]).unwrap();
    let registry = LiveToolOutputRegistry::default();
    let writer = registry.writer_for(doc_id.clone()).await;
    let tool = WriteFileTool::new(ToolContext::new(root_path.clone(), false).unwrap());
    let output = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None, tokio_util::sync::CancellationToken::new(), None, Some(writer), None, None,
        Default::default(), false,
        tool.call(WriteFileArgs {
            path: "file.bin".into(), content: "replacement bytes".into(), raw_json: true,
            expected_content_hash: None, overwrite: true,
        }),
    ).await.unwrap();
    assert!(
        !output.contains("file_changes") && !output.contains("AP+A"),
        "{output}"
    );
    let access = call.access().unwrap();
    let request_doc_id = call.request_doc_id().unwrap().to_owned();
    let session_id = call.session_id().to_owned();
    let requester_did = call.requester_did().map(str::to_owned);
    let actor = call.actor().clone();
    let inventory_admission = ConfigAccess::transact_local_readonly(
        &admission.node,
        Some(actor),
        "test.inventory.session",
        |txn| {
            let (session_id, requester_did, node_did) =
                (&session_id, &requester_did, &admission.node_did);
            Box::pin(async move {
                let session =
                    gents_store::config_client::agent_session::load_agent_session_row_in_txn(
                        txn,
                        node_did,
                        session_id,
                        requester_did.as_deref(),
                    )
                    .await?
                    .unwrap();
                Ok::<_, anyhow::Error>(gents_protocol::file_inventory::FileInventoryAdmission {
                    session_doc_id: session.doc_id,
                    request: gents_protocol::file_inventory::FileInventoryRequest {
                        request_key: "native-inventory".into(),
                        requester_did: requester_did.clone().unwrap(),
                        node_did: node_did.clone(),
                        session_id: session_id.clone(),
                    },
                })
            })
        },
    )
    .await
    .unwrap();
    let pending_inventory = crate::file_inventory::FileInventorySnapshot::load(
        &admission.node,
        &inventory_admission,
    )
    .await
    .unwrap();
    assert_eq!(pending_inventory.binding().coverage.pending, 1);
    assert_eq!(pending_inventory.binding().coverage.unknown, 0);
    assert!(pending_inventory.entries().is_empty());
    let load = |request: String, node: String, session: String, requester: Option<String>| {
        let access = access.clone();
        let doc_id = doc_id.clone();
        async move {
            gents_session::tool_call::query::result::load_tool_call_file_changes(
                &access,
                &doc_id,
                &request,
                &node,
                &session,
                requester.as_deref(),
            )
            .await
        }
    };
    assert!(load(
        request_doc_id.clone(),
        admission.node_did.clone(),
        session_id.clone(),
        requester_did.clone()
    )
    .await
    .unwrap()
    .is_none());
    assert!(matches!(
        gents_session::tool_call::query::result::load_tool_file_change_observation(
            &access,
            &doc_id,
            &request_doc_id,
            &admission.node_did,
            &session_id,
            requester_did.as_deref()
        )
        .await
        .unwrap(),
        gents_session::tool_call::query::result::ToolFileChangeRead::Pending
    ));
    call.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    gents_toolcalls::call::lifecycle::delivery::fail_next_terminal_for_test(&doc_id);
    gents_toolcalls::call::lifecycle::file_inventory::fail_next_publication(&doc_id);
    call.complete(&output).await.unwrap();
    let row = call
        .access()
        .unwrap()
        .execute(&format!(
            "{{ AgentToolCall(docID: \"{}\") {{ lifecycle_state }} ToolFileChange(filter: {{ tool_call_doc_id: {{ _eq: \"{}\" }} }}) {{ _docID file_changes }} ToolFileInventory(filter: {{ tool_call_doc_id: {{ _eq: \"{}\" }} }}) {{ _docID observation_doc_id tool_call_doc_id request_doc_id node_did session_id requester_did changes }} }}",
            escape_graphql_string(&doc_id), escape_graphql_string(&doc_id), escape_graphql_string(&doc_id)
        ))
        .await
        .unwrap()["data"].clone();
    assert_eq!(row["AgentToolCall"][0]["lifecycle_state"], "completed");
    assert_eq!(
        row["ToolFileChange"][0]["file_changes"]["tool_call_doc_id"],
        doc_id
    );
    assert_stored_versions(&row["ToolFileChange"][0]["file_changes"]);
    let wire = serde_json::to_value(
        load(
            request_doc_id.clone(),
            admission.node_did.clone(),
            session_id.clone(),
            requester_did.clone(),
        )
        .await
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    let change = &wire["changes"][0];
    assert_eq!(change["root"], root_path.to_str().unwrap());
    assert_eq!(change["path"], "file.bin");
    assert_eq!(decoded_image(&change["before"]), [0, 255, 128]);
    assert_eq!(decoded_image(&change["after"]), b"replacement bytes");
    let projections = row["ToolFileInventory"].as_array().unwrap();
    assert_eq!(
        projections.len(),
        1,
        "rolled-back publication must leave no projection"
    );
    let projection = &projections[0];
    assert_eq!(
        projection["observation_doc_id"],
        row["ToolFileChange"][0]["_docID"]
    );
    assert_eq!(projection["tool_call_doc_id"], doc_id);
    assert_eq!(projection["request_doc_id"], request_doc_id);
    assert_eq!(projection["node_did"], admission.node_did);
    assert_eq!(projection["session_id"], session_id);
    assert_eq!(
        projection["requester_did"],
        serde_json::json!(requester_did)
    );
    assert_eq!(
        projection["changes"][0],
        serde_json::json!({
            "root": change["root"], "path": change["path"],
            "before": {"kind": "file", "content_hash": change["before"]["content_hash"]},
            "after": {"kind": "file", "content_hash": change["after"]["content_hash"]},
        })
    );
    gents_toolcalls::call::lifecycle::admission_fixture::grant_runtime_projection_readers(
        &access,
        &admission.node_did,
        &session_id,
    )
    .await
    .unwrap();
    let inventory = crate::file_inventory::FileInventorySnapshot::load(
        &admission.node,
        &inventory_admission,
    )
    .await
    .unwrap();
    assert_eq!(inventory.binding().coverage.entries, 1);
    assert_eq!(inventory.binding().coverage.pending, 0);
    assert_eq!(inventory.binding().coverage.unknown, 0);
    assert_ne!(
        inventory.binding().digest,
        pending_inventory.binding().digest
    );
    assert_eq!(
        inventory.entries()[0].projection_doc_id,
        projection["_docID"].as_str().unwrap()
    );
    assert_eq!(inventory.entries()[0].tool_call_doc_id, doc_id);
    assert_eq!(inventory.entries()[0].scope.session_id, session_id);
    assert!(!serde_json::to_string(inventory.entries())
        .unwrap()
        .contains("replacement bytes"));
    let mut wrong_session = inventory_admission.clone();
    wrong_session.session_doc_id = "different-physical-session".into();
    assert!(crate::file_inventory::FileInventorySnapshot::load(
        &admission.node,
        &wrong_session,
    )
    .await
    .is_err());
    let ids = vec![projection["_docID"].as_str().unwrap().to_owned()];
    let owners = ConfigAccess::transact_local_readonly(
        &admission.node,
        Some(admission.node_did.parse().unwrap()),
        "test.file_inventory.owner",
        move |txn| {
            let ids = ids.clone();
            Box::pin(async move { txn.document_owners("ToolFileInventory", &ids).await })
        },
    )
    .await
    .unwrap();
    // The selected Agent signs the projection; the runtime only reads it.
    assert_eq!(
        owners[0].owner.as_ref().map(|owner| owner.as_str()),
        Some(call.actor().as_str())
    );
    let reader = requester_did.as_ref().unwrap().parse().unwrap();
    let denied = ConfigAccess::transact_local_readonly(
        &admission.node,
        Some(reader),
        "test.file_inventory.no_implicit_read",
        |txn| {
            let id = projection["_docID"].as_str().unwrap().to_owned();
            Box::pin(async move {
                txn.execute(&format!(
                    "{{ ToolFileInventory(docID: \"{}\") {{ _docID changes }} }}",
                    escape_graphql_string(&id),
                ))
                .await
            })
        },
    )
    .await
    .unwrap();
    assert!(denied["data"]["ToolFileInventory"]
        .as_array()
        .unwrap()
        .is_empty());
    let content_doc_id = row["ToolFileChange"][0]["file_changes"]["changes"][0]["content_doc_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let reader = requester_did.as_ref().unwrap().parse().unwrap();
    let denied = ConfigAccess::transact_local_readonly(
        &admission.node,
        Some(reader),
        "test.file_content.no_implicit_read",
        |txn| {
            let id = content_doc_id.clone();
            Box::pin(async move {
                txn.execute(&format!(
                    "{{ ToolFileContent(docID: \"{}\") {{ _docID text blob }} }}",
                    escape_graphql_string(&id),
                ))
                .await
            })
        },
    )
    .await
    .unwrap();
    assert!(
        denied["data"]["ToolFileContent"]
            .as_array()
            .unwrap()
            .is_empty(),
        "file content is readable without a receipt grant"
    );

    assert_eq!(std::fs::read(&path).unwrap(), b"replacement bytes");
    let retained = load(
        request_doc_id.clone(),
        admission.node_did.clone(),
        session_id.clone(),
        requester_did.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(serde_json::to_value(&retained).unwrap(), wire);
    let gents_session::tool_call::query::result::ToolFileChangeRead::Retained {
        document_id,
        changes,
        ..
    } = gents_session::tool_call::query::result::load_tool_file_change_observation(
        &access,
        &doc_id,
        &request_doc_id,
        &admission.node_did,
        &session_id,
        requester_did.as_deref(),
    )
    .await
    .unwrap()
    else {
        panic!("expected exact retained observation")
    };
    assert_eq!(row["ToolFileChange"][0]["_docID"], document_id);
    assert_eq!(serde_json::to_value(changes).unwrap(), wire);
    for (request, node, session, requester) in [
        (
            "foreign-request".into(),
            admission.node_did.clone(),
            session_id.clone(),
            requester_did.clone(),
        ),
        (
            request_doc_id.clone(),
            "did:key:foreign-node".into(),
            session_id.clone(),
            requester_did.clone(),
        ),
        (
            request_doc_id.clone(),
            admission.node_did.clone(),
            "foreign-session".into(),
            requester_did.clone(),
        ),
        (
            request_doc_id,
            admission.node_did.clone(),
            session_id,
            Some("did:key:foreign-client".into()),
        ),
    ] {
        assert!(load(request, node, session, requester).await.is_err());
    }
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}
