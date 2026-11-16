use base64::Engine as _;
use gents_loop::live_output::DeferredToolWrites;
use gents_loop::live_output::LiveToolOutputRegistry;
use gents_loop::tool::Tool;
use gents_protocol::file_changes::{
    FileChangeImages as FileChange, FileImage, ToolFileChanges as ChangeBatch,
    MAX_FILE_IMAGE_BYTES as MAX_IMAGE_BYTES,
};
use gents_session::file_contents::{content_hash, FileChanges};
use gents_store::graphql::escape_graphql_string;
use gents_toolcalls::call::lifecycle::admission_fixture::{
    published_admission, PublishedAdmissionOptions,
};
use gents_tools::native::args::WriteFileArgs;
use gents_tools::native::file_tools::change_capture::*;
use gents_tools::native::file_tools::file_mutation_lock_for;
use gents_tools::native::file_tools::write_file::WriteFileTool;
use gents_tools::native::shared::ToolContext;
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

#[tokio::test]
async fn readonly_receipts_commit_without_images_and_survive_terminal_retry() {
    use gents_loop::tool_call_lifecycle::runtime::{call_tool_managed, ToolOutcome};
    use gents_tools::native::file_tools::read_tools::ReadFileTool;

    let admission = published_admission(PublishedAdmissionOptions {
        name: "readonly-capture-terminal".into(),
        real_identity: true,
        tool_name: Some("read_file".into()),
        paired_client: true,
        composition: gents_session::test_support::no_lifecycle_plugins(),
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
        None,
        None, tokio_util::sync::CancellationToken::new(), None, Some(writer), None, None,
        Default::default(), false,
        call_tool_managed(&tool, serde_json::json!({"path": "source.txt", "raw_json": true}).to_string()),
    ).await;
    let ToolOutcome::Completed(output) = outcome else {
        panic!("read failed: {outcome:?}")
    };
    assert!(!output.contains("verified_unchanged"));
    call.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    let mut replay = gents_toolcalls::call::lifecycle::ToolCallLifecycle::load_by_doc_id_as(
        admission.node.clone(),
        call.actor().clone(),
        &doc_id,
        &admission.node_did,
        call.session_id(),
        call.requester_did(),
    )
    .await
    .unwrap()
    .unwrap();
    // A rehydrated row terminates through its owning Agent's signer.
    replay
        .bind_agent_signer(call.access().unwrap().managed_agent_signer())
        .unwrap();
    replay.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    gents_toolcalls::call::lifecycle::delivery::fail_next_terminal_for_test(&doc_id);
    gents_toolcalls::call::lifecycle::file_inventory::fail_next_publication(&doc_id);
    call.complete(&output).await.unwrap();
    replay.complete(&output).await.unwrap();
    let row = call.access().unwrap().execute(&format!(
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

#[tokio::test]
async fn maximum_images_survive_the_terminal_transaction_and_scoped_read() {
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-change-maximum-images".into(),
        real_identity: true,
        tool_name: Some("write_file".into()),
        paired_client: true,
        composition: gents_session::test_support::no_lifecycle_plugins(),
        ..Default::default()
    })
    .await
    .unwrap();
    admission.grant_agent_collection_get().await.unwrap();
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
        None,
        None, tokio_util::sync::CancellationToken::new(), None, Some(writer), None, None,
        Default::default(), false,
        tool.call(WriteFileArgs {
            path: "maximum.txt".into(), content: after.clone(), raw_json: true,
            expected_content_hash: None, overwrite: true,
        }),
    ).await.unwrap();
    call.stage_deferred_writes(registry.deferred_writes(&doc_id).await);
    call.complete(&output).await.unwrap();
    let retained = gents_session::tool_call::query::result::load_tool_call_file_changes(
        &call.access().unwrap(),
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
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}

#[tokio::test]
async fn failed_tool_retains_partial_effects_and_rejects_foreign_images() {
    use gents_loop::tool_call_lifecycle::FailureClass;
    let admission = published_admission(PublishedAdmissionOptions {
        name: "file-change-failed-terminal".into(),
        real_identity: true,
        tool_name: Some("write_file".into()),
        composition: gents_session::test_support::no_lifecycle_plugins(),
        ..Default::default()
    })
    .await
    .unwrap();
    admission.grant_agent_collection_get().await.unwrap();
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
    let foreign = FileChanges::new(ChangeBatch {
        version: 1,
        tool_call_doc_id: "foreign-tool".into(),
        changes: vec![FileChange {
            root: root_path.to_str().unwrap().into(),
            path: "file".into(),
            before: FileImage::Missing,
            after: read_image(&path),
        }],
    });
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
    let access = call.access().unwrap();
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
        gents_session::tool_call::query::result::load_tool_call_file_changes(
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
    admission
        .node
        .shutdown()
        .await
        .expect("shut down test node");
    std::fs::remove_dir_all(admission.path).unwrap();
}
