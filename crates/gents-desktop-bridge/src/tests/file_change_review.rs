use gents_protocol::file_changes::{
    FileChangeImages, FileImage, FilePermissions, ToolFileChanges, FILE_CHANGES_VERSION,
    MAX_FILE_IMAGE_BYTES,
};
use gents_protocol::job::{JobBudget, JobProgress, JobState};

use gents_store::graphql::escape_graphql_string;

use super::operations_job_control::seed_admission_for_node;
use super::support::{create_session, seed_job_row_for_node, OPERATOR};

#[tokio::test]
async fn file_change_review_reads_the_bound_request_without_touching_a_local_checkout() {
    let (core, _dir) = boot_review_core().await;
    let node = core.runtime_connections()[0].node_did.clone();
    let session = "file-review-session";
    let call = "file-review-call";
    let request = format!("req-{call}");
    create_session(&core, &node, session, None).await;
    let (physical, expected) = seed_review(
        &core,
        session,
        call,
        FileImage::Missing,
        FileImage::File {
            content_hash: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .into(),
            data: String::new(),
            permissions: FilePermissions {
                readonly: false,
                unix_mode: None,
            },
            links: None,
        },
    )
    .await;
    let progress = JobProgress {
        title: "File change".into(),
        state: JobState::Completed,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 0,
        budget: JobBudget {
            total: 1,
            spent: 0,
            remaining: 1,
        },
        last_event: "finished".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let result = crate::commands::tool_file_changes(
        &core,
        node.clone(),
        session.into(),
        request.clone(),
        physical.clone(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result, Some(expected.clone()));
    assert!(crate::commands::tool_file_changes(
        &core,
        node.clone(),
        session.into(),
        request.clone(),
        physical.clone(),
        Some("not-a-uuid".into()),
    )
    .await
    .is_err());
    let other = seed_job_row_for_node(
        &core,
        &node,
        session,
        "foreign-call",
        "completed",
        &progress,
        None,
    )
    .await;
    seed_admission_for_node(&core, &node, session, &other, "foreign-call", 2).await;
    assert!(crate::commands::tool_file_changes(
        &core,
        node.clone(),
        session.into(),
        "req-foreign-call".into(),
        physical.clone(),
        None,
    )
    .await
    .is_err());
    assert!(crate::commands::tool_file_changes(
        &core,
        node.clone(),
        session.into(),
        request.clone(),
        call.into(),
        None,
    )
    .await
    .is_err());
    for (target, other_session) in [
        ("foreign-node", session),
        (node.as_str(), "foreign-session"),
    ] {
        assert!(!matches!(
            crate::commands::tool_file_changes(
                &core,
                target.into(),
                other_session.into(),
                request.clone(),
                physical.clone(),
                None,
            )
            .await,
            Ok(Some(_))
        ));
    }
    let access = core
        .operator_access(&core.runtime_connections()[0].node_did)
        .unwrap();
    let observation = access.execute(&format!(
        r#"{{ ToolFileChange(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}) {{ _docID tool_call_doc_id request_doc_id node_did session_id requester_did file_changes }} }}"#,
        escape_graphql_string(&physical),
    )).await.unwrap();
    let original = &observation["data"]["ToolFileChange"][0];
    let mutation = format!(
        r#"mutation {{ update_ToolFileChange(docID: "{}", input: {{ session_id: "another-session" }}) {{ _docID }} }}"#,
        escape_graphql_string(original["_docID"].as_str().unwrap()),
    );
    assert!(access
        .write("test.file_review.immutable", &mutation)
        .await
        .is_err());
    assert_eq!(
        crate::commands::tool_file_changes(
            &core,
            node.clone(),
            session.into(),
            request.clone(),
            physical.clone(),
            None,
        )
        .await
        .unwrap(),
        Some(expected)
    );

    let mut foreign = original.clone();
    foreign.as_object_mut().unwrap().remove("_docID");
    foreign["requester_did"] = serde_json::json!("did:key:another-requester");
    let variables = serde_json::json!({"input": foreign});
    access.transact_write("test.file_review.ambiguous", |txn| {
        let variables = &variables;
        Box::pin(async move {
            txn.execute_with_variables(
                "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                variables,
            ).await
        })
    }).await.unwrap();
    let error = crate::commands::tool_file_changes(
        &core,
        node.clone(),
        session.into(),
        request,
        physical,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("ambiguous observations"),
        "{error:#}"
    );
    core.shutdown().await.unwrap();
}

async fn seed_review(
    core: &std::sync::Arc<crate::client_runtime::ClientRuntime>,
    session: &str,
    call: &str,
    before: FileImage,
    after: FileImage,
) -> (String, ToolFileChanges) {
    let progress = JobProgress {
        title: "File change".into(),
        state: JobState::Completed,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 0,
        budget: JobBudget {
            total: 1,
            spent: 0,
            remaining: 1,
        },
        last_event: "finished".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let node = &core.runtime_connections()[0].node_did;
    let physical =
        seed_job_row_for_node(core, node, session, call, "completed", &progress, None).await;
    seed_admission_for_node(core, node, session, &physical, call, 1).await;
    let expected = ToolFileChanges {
        version: FILE_CHANGES_VERSION,
        tool_call_doc_id: physical.clone(),
        changes: vec![FileChangeImages {
            root: "/unmapped/runtime/workspace".into(),
            path: "empty.txt".into(),
            before,
            after,
        }],
    };
    let access = core
        .operator_access(&core.runtime_connections()[0].node_did)
        .unwrap();
    let binding = access
        .execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ request_doc_id }} }}"#,
            escape_graphql_string(&physical),
        ))
        .await
        .unwrap();
    let mutation = "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }";
    let variables = serde_json::json!({"input": {
        "tool_call_doc_id": physical, "request_doc_id": binding["data"]["AgentToolCall"][0]["request_doc_id"],
        "node_did": node, "session_id": session, "requester_did": OPERATOR,
        "file_changes": expected,
    }});
    access
        .transact_write("test.file_review.images", |txn| {
            let (mutation, variables) = (mutation, &variables);
            Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
        })
        .await
        .unwrap();
    (physical, expected)
}

#[tokio::test]
async fn concurrent_file_reviews_preserve_each_retained_image() {
    let (core, _directory) = boot_review_core().await;
    let node = core.runtime_connections()[0].node_did.clone();
    let session = "concurrent-file-review";
    let call = "concurrent-file-call";
    create_session(&core, &node, session, None).await;
    let (physical, expected) =
        seed_review(&core, session, call, FileImage::Missing, FileImage::Missing).await;
    let request = format!("req-{call}");
    let results = futures::future::join_all((0..5).map(|_| {
        crate::commands::tool_file_changes(
            &core,
            node.clone(),
            session.into(),
            request.clone(),
            physical.clone(),
            None,
        )
    }))
    .await;
    for result in results {
        assert_eq!(result.unwrap(), Some(expected.clone()));
    }
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn unavailable_file_review_scope_is_retryable_not_missing_versions() {
    let (core, _directory) = boot_review_core().await;
    let node = core.runtime_connections()[0].node_did.clone();
    let error = crate::commands::tool_file_changes(
        &core,
        node,
        "not-received-session".into(),
        "not-received-request".into(),
        "physical-tool".into(),
        None,
    )
    .await
    .unwrap_err();
    let classified = error.downcast_ref::<crate::BridgeError>().unwrap();
    assert_eq!(classified.code, crate::BridgeErrorCode::ProjectionPending);
    assert!(classified.retryable);
    core.shutdown().await.unwrap();
}

#[tokio::test]
async fn maximum_file_images_cross_the_editor_host_frames_without_truncation() {
    use crate::host::{protocol, ClientHost};
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use std::{sync::Arc, time::Duration};
    use tokio_util::{codec::LengthDelimitedCodec, sync::CancellationToken};

    let (core, _dir) = boot_review_core().await;
    let node = core.runtime_connections()[0].node_did.clone();
    let session = "file-review-maximum";
    let call = "maximum-file-call";
    create_session(&core, &node, session, None).await;
    let image = |triplet: &str, tail: &str, hash: &str| FileImage::File {
        content_hash: format!("sha256:{hash}"),
        data: triplet.repeat(MAX_FILE_IMAGE_BYTES as usize / 3) + tail,
        permissions: FilePermissions {
            readonly: false,
            unix_mode: None,
        },
        links: None,
    };
    assert_eq!(MAX_FILE_IMAGE_BYTES % 3, 1);
    let (physical, expected) = seed_review(
        &core,
        session,
        call,
        image(
            "YWFh",
            "YQ==",
            "9bc1b2a288b26af7257a36277ae3816a7d4f16e89c1e7e77d0a5c48bad62b360",
        ),
        image(
            "YmJi",
            "Yg==",
            "e56ec8dc1862be6c09c53620cbc0f00f639de2a51c882745fbbc4e144714b3c2",
        ),
    )
    .await;
    let (client, server) = tokio::io::duplex(4096);
    let stop = CancellationToken::new();
    let host = Arc::new(ClientHost::new(core.clone()));
    let task = tokio::spawn(host.serve(server, stop.clone()));
    let mut client = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(client);
    client
        .send(Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "kind": "hello", "version": protocol::PROTOCOL_VERSION,
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let hello: serde_json::Value =
        serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(hello["version"], protocol::PROTOCOL_VERSION);
    client
        .send(Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "kind": "call", "id": 1, "command": {
                    "name": "desktop_tool_file_changes", "args": {
                        "nodeDid": node, "sessionId": session,
                        "requestId": format!("req-{call}"), "toolCallDocId": physical,
                        "retryId": "59761a54-a2e2-4bb1-9a08-ec0df44fd4c1",
                    },
                },
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = client.next().await.unwrap().unwrap();
            let response: serde_json::Value = serde_json::from_slice(&frame).unwrap();
            if response["kind"] == "response" {
                assert!(response["error"].is_null(), "{response}");
                assert!(frame.len() > MAX_FILE_IMAGE_BYTES as usize * 2);
                break response;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(response["id"], 1);
    assert!(response["error"].is_null());
    assert_eq!(
        serde_json::from_value::<ToolFileChanges>(response["result"].clone()).unwrap(),
        expected
    );
    stop.cancel();
    drop(client);
    task.await.unwrap().unwrap();
    core.shutdown().await.unwrap();
}

struct ReviewFixture {
    _directory: tempfile::TempDir,
    _runtime: gents::client_test_support::ServedRuntimeHome,
}

async fn boot_review_core() -> (
    std::sync::Arc<crate::client_runtime::ClientRuntime>,
    ReviewFixture,
) {
    let directory = tempfile::tempdir().unwrap();
    let core = std::sync::Arc::new(
        crate::client_runtime::ClientRuntime::start_with_paths_and_options(
            gents_desktop_core::client::DesktopPaths::from_root(directory.path()),
            crate::client_test_support::file_journal_options(),
        )
        .await
        .unwrap(),
    );
    let runtime = gents::client_test_support::ServedRuntimeHome::start_unpublished_for(
        std::sync::Arc::new(core.node_identity().clone()),
    )
    .await;
    runtime.publish_node(serde_json::json!({})).await;
    crate::client_test_support::connect_runtime(&runtime, &core).await;
    (
        core,
        ReviewFixture {
            _directory: directory,
            _runtime: runtime,
        },
    )
}
