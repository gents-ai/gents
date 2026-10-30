use super::*;
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::llm::tool::Tool;
use crate::tool_call_lifecycle::admission_fixture::{
    published_admission, PublishedAdmissionOptions,
};
use crate::toolset::args::WriteFileArgs;
use crate::toolset::file_tools::file_mutation_lock_for;
use crate::toolset::file_tools::write_file::WriteFileTool;
use crate::toolset::shared::ToolContext;
use gents_loop::live_output::LiveToolOutputRegistry;
use serde_json::Value;

fn decoded_image(value: &Value) -> Vec<u8> {
    assert_eq!(value["kind"], "file");
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(value["content_hash"], content_hash(&bytes));
    bytes
}

/// Stored observations name file-document versions and carry no bytes.
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

#[test]
fn images_distinguish_missing_empty_binary_and_unavailable_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.bin");
    assert_eq!(
        serde_json::to_value(read_image(&path)).unwrap()["kind"],
        "missing"
    );
    std::fs::write(&path, []).unwrap();
    assert!(decoded_image(&serde_json::to_value(read_image(&path)).unwrap()).is_empty());
    std::fs::write(&path, [0, 255, 128, 13, 10]).unwrap();
    assert_eq!(
        decoded_image(&serde_json::to_value(read_image(&path)).unwrap()),
        [0, 255, 128, 13, 10]
    );
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_IMAGE_BYTES + 1).unwrap();
    let large = serde_json::to_value(read_image(&path)).unwrap();
    assert_eq!(large["kind"], "unavailable");
    assert_eq!(large["reason"], "tooLarge");
    assert!(large.get("data").is_none());
    assert_eq!(
        serde_json::to_value(read_image(root.path())).unwrap()["reason"],
        "unsupportedEntry"
    );
}

#[cfg(unix)]
#[test]
fn images_preserve_permission_observations_and_refuse_symlink_content() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    std::fs::write(&path, "private bytes").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
    let value = serde_json::to_value(read_image(&path)).unwrap();
    assert_eq!(value["permissions"]["unix_mode"], 0o751);
    assert_eq!(value["links"], 1);
    let link = root.path().join("link");
    symlink(&path, &link).unwrap();
    let value = serde_json::to_value(read_image(&link)).unwrap();
    assert_eq!(value["reason"], "unsupportedEntry");
    assert!(value.get("data").is_none());
}

#[tokio::test]
async fn native_images_commit_with_their_physical_terminal_and_stay_out_of_tool_output() {
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-change-native-terminal".into(),
        real_identity: true,
        tool_name: Some("write_file".into()),
        paired_client: true,
        ..Default::default()
    })
    .await
    .unwrap();
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
    let access = ConfigAccess::local(admission.node.clone());
    let request_doc_id = call.request_doc_id().unwrap().to_owned();
    let session_id = call.session_id().to_owned();
    let requester_did = call.requester_did().map(str::to_owned);
    let inventory_admission = ConfigAccess::transact_local_readonly(
        &admission.node,
        None,
        "test.inventory.session",
        |txn| {
            let (session_id, requester_did, node_did) =
                (&session_id, &requester_did, &admission.node_did);
            Box::pin(async move {
                let session = crate::session::load_agent_session_row_in_txn(
                    txn,
                    node_did,
                    session_id,
                    requester_did.as_deref(),
                )
                .await?
                .unwrap();
                Ok::<_, anyhow::Error>(gents_protocol::file_inventory::FileInventoryAdmission {
                    session_doc_id: session.doc_id,
                    request: gents_protocol::session_hydration::HydrationRequest {
                        request_key: "native-inventory".into(),
                        peer_id: "native-peer".into(),
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
    let pending_inventory =
        crate::file_inventory::FileInventorySnapshot::load(&admission.node, &inventory_admission)
            .await
            .unwrap();
    assert_eq!(pending_inventory.binding().coverage.pending, 1);
    assert_eq!(pending_inventory.binding().coverage.unknown, 0);
    assert!(pending_inventory.entries().is_empty());
    let load = |request: String, node: String, session: String, requester: Option<String>| {
        let access = access.clone();
        let doc_id = doc_id.clone();
        async move {
            crate::tool_call_lifecycle::query::load_tool_call_file_changes(
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
        crate::tool_call_lifecycle::load_tool_file_change_observation(
            &access,
            &doc_id,
            &request_doc_id,
            &admission.node_did,
            &session_id,
            requester_did.as_deref()
        )
        .await
        .unwrap(),
        crate::tool_call_lifecycle::ToolFileChangeRead::Pending
    ));
    call.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    crate::tool_call_lifecycle::delivery::fail_next_terminal_for_test(&doc_id);
    crate::tool_call_lifecycle::file_inventory::fail_next_publication(&doc_id);
    call.complete(&output).await.unwrap();
    let row = ConfigAccess::local(admission.node.clone())
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
    let inventory =
        crate::file_inventory::FileInventorySnapshot::load(&admission.node, &inventory_admission)
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
    assert!(
        crate::file_inventory::FileInventorySnapshot::load(&admission.node, &wrong_session,)
            .await
            .is_err()
    );
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
    assert_eq!(
        owners[0].owner.as_ref().map(|owner| owner.as_str()),
        Some(admission.node_did.as_str())
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
    let crate::tool_call_lifecycle::ToolFileChangeRead::Retained {
        document_id,
        changes,
        ..
    } = crate::tool_call_lifecycle::load_tool_file_change_observation(
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
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn maximum_images_survive_the_terminal_transaction_and_scoped_read() {
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-change-maximum-images".into(),
        real_identity: true,
        tool_name: Some("write_file".into()),
        paired_client: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let mut call = admission.tool;
    let doc_id = call.doc_id().unwrap().to_owned();
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("maximum.txt");
    let before = "a".repeat(MAX_IMAGE_BYTES as usize);
    let after = "b".repeat(MAX_IMAGE_BYTES as usize);
    std::fs::write(&path, &before).unwrap();
    let registry = LiveToolOutputRegistry::default();
    let writer = registry.writer_for(doc_id.clone()).await;
    let tool = WriteFileTool::new(ToolContext::new(root_path, false).unwrap());
    let output = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None, tokio_util::sync::CancellationToken::new(), None, Some(writer), None, None,
        Default::default(), false,
        tool.call(WriteFileArgs {
            path: "maximum.txt".into(), content: after.clone(), raw_json: true,
            expected_content_hash: None, overwrite: true,
        }),
    ).await.unwrap();
    call.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    call.complete(&output).await.unwrap();
    let retained = crate::tool_call_lifecycle::load_tool_call_file_changes(
        &ConfigAccess::local(admission.node.clone()),
        &doc_id,
        call.request_doc_id().unwrap(),
        &admission.node_did,
        call.session_id(),
        call.requester_did(),
    )
    .await
    .unwrap()
    .unwrap();
    let value = serde_json::to_value(retained).unwrap();
    let images = &value["changes"][0];
    assert_eq!(decoded_image(&images["before"]), before.as_bytes());
    assert_eq!(decoded_image(&images["after"]), after.as_bytes());
    assert_eq!(std::fs::read(&path).unwrap(), after.as_bytes());
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn incomplete_images_retain_the_original_without_claiming_a_finished_mutation() {
    let registry = LiveToolOutputRegistry::default();
    let writer = registry.writer_for("physical-tool").await;
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("file");
    std::fs::write(&path, "original bytes").unwrap();
    gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None,
        tokio_util::sync::CancellationToken::new(),
        None,
        Some(writer),
        None,
        None,
        Default::default(),
        false,
        async {
            let lock = file_mutation_lock_for(&path);
            let _guard = lock.lock().await;
            let capture = FileChangeCapture::start(&root_path, &path).await.unwrap();
            std::fs::write(&path, "partial bytes").unwrap();
            drop(capture);
        },
    )
    .await;
    let pending = registry.deferred_writes("physical-tool").await.unwrap();
    let changes = serde_json::to_value(pending.downcast_ref::<FileChanges>().unwrap()).unwrap();
    assert_eq!(
        decoded_image(&changes["changes"][0]["before"]),
        b"original bytes"
    );
    assert_eq!(
        changes["changes"][0]["after"]["reason"],
        "mutationIncomplete"
    );
}

#[tokio::test]
async fn previews_and_rejected_edits_do_not_publish_file_images() {
    use crate::toolset::args::EditFileArgs;
    use crate::toolset::file_tools::edit_file::EditFileTool;
    let registry = LiveToolOutputRegistry::default();
    let writer = registry.writer_for("preview-tool").await;
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("file");
    std::fs::write(&path, b"original\r\nbytes\r\n").unwrap();
    let tool = EditFileTool::new(ToolContext::new(root_path.clone(), false).unwrap());
    gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None,
        tokio_util::sync::CancellationToken::new(),
        None,
        Some(writer),
        None,
        None,
        Default::default(),
        false,
        async {
            let args = |dry_run, expected_content_hash| EditFileArgs {
                path: "file".into(),
                old_text: "original".into(),
                new_text: "updated".into(),
                replace_all: false,
                raw_json: true,
                dry_run,
                match_mode: None,
                operation: None,
                expected_content_hash,
            };
            tool.call(args(true, None)).await.unwrap();
            assert!(registry.deferred_writes("preview-tool").await.is_none());
            assert_eq!(std::fs::read(&path).unwrap(), b"original\r\nbytes\r\n");
            assert!(tool.call(args(false, Some("stale".into()))).await.is_err());
            assert!(registry.deferred_writes("preview-tool").await.is_none());
            tool.call(args(false, None)).await.unwrap();
        },
    )
    .await;
    let writes = registry.deferred_writes("preview-tool").await.unwrap();
    let changes = serde_json::to_value(writes.downcast_ref::<FileChanges>().unwrap()).unwrap();
    assert_eq!(
        decoded_image(&changes["changes"][0]["before"]),
        b"original\r\nbytes\r\n"
    );
    assert_eq!(
        decoded_image(&changes["changes"][0]["after"]),
        b"updated\r\nbytes\r\n"
    );
}

#[tokio::test]
async fn failed_tool_retains_partial_effects_and_rejects_foreign_images() {
    use crate::tool_call_lifecycle::FailureClass;
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-change-failed-terminal".into(),
        real_identity: true,
        tool_name: Some("write_file".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let mut call = admission.tool;
    let doc_id = call.doc_id().unwrap().to_owned();
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("file");
    std::fs::write(&path, "original").unwrap();
    let registry = LiveToolOutputRegistry::default();
    let writer = registry.writer_for(doc_id.clone()).await;
    gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None,
        tokio_util::sync::CancellationToken::new(),
        None,
        Some(writer),
        None,
        None,
        Default::default(),
        false,
        async {
            let lock = file_mutation_lock_for(&path);
            let _guard = lock.lock().await;
            let capture = FileChangeCapture::start(&root_path, &path).await.unwrap();
            std::fs::write(&path, "partial").unwrap();
            drop(capture);
        },
    )
    .await;
    let foreign = FileChanges(Arc::new(ChangeBatch {
        version: 1,
        tool_call_doc_id: "foreign-tool".into(),
        changes: vec![FileChange {
            root: root_path.to_str().unwrap().into(),
            path: "file".into(),
            before: FileImage::Missing,
            after: read_image(&path),
        }],
    }));
    call.stage_deferred_writes(Some(DeferredToolWrites::new(foreign)));
    assert!(call
        .fail("write failed", FailureClass::ToolReturnedError)
        .await
        .unwrap_err()
        .to_string()
        .contains("another physical tool"));
    let query = format!(
        "{{ AgentToolCall(docID: \"{}\") {{ lifecycle_state }} ToolFileChange(filter: {{ tool_call_doc_id: {{ _eq: \"{}\" }} }}) {{ file_changes }} ToolFileInventory(filter: {{ tool_call_doc_id: {{ _eq: \"{}\" }} }}) {{ _docID observation_doc_id changes }} }}",
        escape_graphql_string(&doc_id), escape_graphql_string(&doc_id), escape_graphql_string(&doc_id)
    );
    let access = ConfigAccess::local(admission.node.clone());
    let row = access.execute(&query).await.unwrap()["data"].clone();
    assert_eq!(row["AgentToolCall"][0]["lifecycle_state"], "running");
    assert_eq!(row["ToolFileChange"].as_array().unwrap().len(), 0);
    assert_eq!(row["ToolFileInventory"].as_array().unwrap().len(), 0);
    call.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    let mut competing = call.clone();
    call.fail("write failed", FailureClass::ToolReturnedError)
        .await
        .unwrap();
    let row = access.execute(&query).await.unwrap()["data"].clone();
    assert_eq!(row["AgentToolCall"][0]["lifecycle_state"], "failed");
    assert_stored_versions(&row["ToolFileChange"][0]["file_changes"]);
    let wire = serde_json::to_value(
        crate::tool_call_lifecycle::query::load_tool_call_file_changes(
            &access,
            &doc_id,
            call.request_doc_id().unwrap(),
            &admission.node_did,
            call.session_id(),
            call.requester_did(),
        )
        .await
        .unwrap()
        .unwrap(),
    )
    .unwrap();
    let change = &wire["changes"][0];
    assert_eq!(decoded_image(&change["before"]), b"original");
    assert_eq!(change["after"]["reason"], "mutationIncomplete");
    assert_eq!(row["ToolFileInventory"].as_array().unwrap().len(), 1);
    assert_eq!(
        row["ToolFileInventory"][0]["changes"][0]["after"],
        serde_json::json!({
            "kind": "unavailable", "reason": "mutationIncomplete",
        })
    );

    std::fs::write(&path, "later user edit").unwrap();
    competing.complete("write failed").await.unwrap();
    assert_eq!(access.execute(&query).await.unwrap()["data"], row);
    assert_eq!(std::fs::read(&path).unwrap(), b"later user edit");
    admission.node.shutdown().await;
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[test]
fn retained_images_reject_corruption_unsupported_versions_and_ambiguous_paths() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    std::fs::write(&path, "observed bytes").unwrap();
    let changes = ChangeBatch {
        version: FILE_CHANGES_VERSION,
        tool_call_doc_id: "physical-tool".into(),
        changes: vec![FileChange {
            root: root.path().to_str().unwrap().into(),
            path: "file".into(),
            before: FileImage::Missing,
            after: read_image(&path),
        }],
    };
    let valid = serde_json::to_value(changes).unwrap();
    assert!(FileChanges::decode("physical-tool", valid.clone()).is_ok());
    let invalid = [
        ("/version", serde_json::json!(FILE_CHANGES_VERSION + 1)),
        ("/tool_call_doc_id", serde_json::json!("foreign-tool")),
        ("/changes/0/path", serde_json::json!("../outside")),
        ("/changes/0/root", serde_json::json!("")),
        ("/changes/0/after/content_hash", serde_json::json!("wrong")),
        ("/changes/0/after/data", serde_json::json!("not base64")),
        (
            "/changes/0/after/permissions/unix_mode",
            serde_json::json!(0o10000),
        ),
        ("/changes/0/after/links", serde_json::json!(0)),
        ("/changes", serde_json::json!([])),
        (
            "/changes",
            serde_json::json!([valid["changes"][0], valid["changes"][0]]),
        ),
    ];
    for (pointer, value) in invalid {
        let mut changed = valid.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            FileChanges::decode("physical-tool", changed).is_err(),
            "{pointer}"
        );
    }
    let mut oversized = valid;
    oversized["changes"][0]["after"]["data"] =
        serde_json::json!("A".repeat((MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4) as usize));
    assert!(FileChanges::decode("physical-tool", oversized).is_err());
}

#[test]
fn replaced_directory_entry_cannot_reuse_an_open_file_image() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    std::fs::write(&path, "same bytes").unwrap();
    let opened = open_regular_file(&path).unwrap();
    let original = opened.metadata().unwrap();
    std::fs::rename(&path, root.path().join("retained-original")).unwrap();
    std::fs::write(&path, "same bytes").unwrap();
    let replacement = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    replacement
        .set_modified(original.modified().unwrap())
        .unwrap();
    let current = replacement.metadata().unwrap();
    assert_eq!(original.len(), current.len());
    assert_eq!(original.modified().unwrap(), current.modified().unwrap());
    let image = serde_json::to_value(read_open_image(&path, opened).unwrap()).unwrap();
    assert_eq!(image["reason"], "changedDuringRead");
    assert!(image.get("data").is_none());
}

#[test]
fn unlinked_directory_entry_cannot_be_reported_as_a_captured_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    std::fs::write(&path, "retained descriptor bytes").unwrap();
    let opened = open_regular_file(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    let image = serde_json::to_value(read_open_image(&path, opened).unwrap()).unwrap();
    assert_eq!(image["reason"], "changedDuringRead");
    assert!(image.get("data").is_none());
}

#[cfg(unix)]
#[test]
fn changed_inode_metadata_is_detected_with_unchanged_modification_time() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file");
    std::fs::write(&path, "original").unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let before = file.metadata().unwrap();
    std::fs::hard_link(&path, root.path().join("temporary-link")).unwrap();
    let after = file.metadata().unwrap();
    assert!(!image_metadata_unchanged(&before, &after).unwrap());
}

#[tokio::test]
async fn readonly_receipts_commit_without_images_and_survive_terminal_retry() {
    use crate::toolset::file_tools::read_tools::ReadFileTool;
    use gents_loop::tool_call_lifecycle::runtime::{call_tool_managed, ToolOutcome};

    let admission = published_admission(PublishedAdmissionOptions {
        name: "readonly-capture-terminal".into(),
        real_identity: true,
        tool_name: Some("read_file".into()),
        paired_client: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let mut call = admission.tool;
    let doc_id = call.doc_id().unwrap().to_owned();
    let root = tempfile::tempdir().unwrap();
    let root_path = std::fs::canonicalize(root.path()).unwrap();
    let path = root_path.join("source.txt");
    std::fs::write(&path, "unchanged bytes\n").unwrap();
    let tool = ReadFileTool::new(ToolContext::new(root_path, false).unwrap(), 4096);
    let registry = LiveToolOutputRegistry::default();
    let writer = registry.writer_for(doc_id.clone()).await;
    let outcome = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
        None, tokio_util::sync::CancellationToken::new(), None, Some(writer), None, None,
        Default::default(), false,
        call_tool_managed(&tool, serde_json::json!({"path": "source.txt", "raw_json": true}).to_string()),
    ).await;
    let ToolOutcome::Completed(output) = outcome else {
        panic!("read failed: {outcome:?}")
    };
    assert!(!output.contains("verified_unchanged"));
    call.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    let mut replay = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
        admission.node.clone(),
        &doc_id,
        &admission.node_did,
        call.session_id(),
        call.requester_did(),
    )
    .await
    .unwrap()
    .unwrap();
    replay.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    crate::tool_call_lifecycle::delivery::fail_next_terminal_for_test(&doc_id);
    crate::tool_call_lifecycle::file_inventory::fail_next_publication(&doc_id);
    call.complete(&output).await.unwrap();
    replay.complete(&output).await.unwrap();
    let row = ConfigAccess::local(admission.node.clone()).execute(&format!(
        "{{ ToolFileChange(filter: {{ tool_call_doc_id: {{ _eq: \"{}\" }} }}) {{ _docID }} ToolFileInventory(filter: {{ tool_call_doc_id: {{ _eq: \"{}\" }} }}) {{ _docID observation_doc_id request_doc_id verified_unchanged changes }} }}",
        escape_graphql_string(&doc_id), escape_graphql_string(&doc_id),
    )).await.unwrap()["data"].clone();
    assert!(row["ToolFileChange"].as_array().unwrap().is_empty());
    let rows = row["ToolFileInventory"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "retry and replay must retain one witness");
    assert_eq!(rows[0]["verified_unchanged"], true);
    assert_eq!(rows[0]["request_doc_id"], call.request_doc_id().unwrap());
    assert!(rows[0]["observation_doc_id"].is_null());
    assert!(rows[0]["changes"].is_null());
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged bytes\n");

    let failed = gents_loop::tool::ToolDyn::call_with_receipt(
        &tool,
        serde_json::json!({"path": "missing.txt"}).to_string(),
    )
    .await;
    assert!(failed.result.is_err());
    assert!(failed.deferred_writes.is_none());
}
