use gents_protocol::file_changes::{
    FileChangeImages, FileImage, FilePermissions, ToolFileChanges, FILE_CHANGES_VERSION,
    MAX_FILE_IMAGE_BYTES,
};
use gents_protocol::job::JobState;
use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadRef, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
    TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};

use gents_store::graphql::escape_graphql_string;

use super::support::{
    attach_host, fixture_job_progress, seed_served_job_row, served_desktop, ServedDesktop,
};

#[tokio::test]
async fn file_change_reviews_enforce_scope_concurrency_retryability_and_max_host_frames() {
    use crate::host::{protocol, ClientHost};
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use std::{sync::Arc, time::Duration};
    use tokio_util::sync::CancellationToken;

    let served = served_desktop().await;
    let core = served.core.clone();
    let node = core.runtime_connections()[0].node_did.clone();

    // Bound request read, invalid scope/retry rejections, immutability, and ambiguity.
    let session = "file-review-session";
    let call = "file-review-call";
    let request = format!("req-{call}");
    let (physical, expected) = seed_review(
        &served,
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
    let progress = fixture_job_progress("File change", JobState::Completed, None);
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
    let other = seed_served_job_row(
        &served,
        session,
        "foreign-call",
        "completed",
        &progress,
        None,
    )
    .await;
    seed_admission(&served, session, &other, "foreign-call", 2).await;
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
    let image_access = served.runtime.agent_access(&served.agent).await;
    let reader = core.node_identity().did().parse().unwrap();
    image_access.transact_write("test.file_review.ambiguous", |txn| {
        let (variables, reader) = (&variables, &reader);
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                variables,
            ).await?;
            let observation = gents_store::graphql::created_doc_id(&response, "ToolFileChange")?;
            txn.add_actor_relationship("ToolFileChange", &observation, "reader", reader).await?;
            Ok::<_, anyhow::Error>(())
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

    // Concurrent reads preserve each retained image.
    let concurrent_session = "concurrent-file-review";
    let concurrent_call = "concurrent-file-call";
    let (concurrent_doc, concurrent_expected) = seed_review(
        &served,
        concurrent_session,
        concurrent_call,
        FileImage::Missing,
        FileImage::Missing,
    )
    .await;
    let concurrent_request = format!("req-{concurrent_call}");
    let results = futures::future::join_all((0..5).map(|_| {
        crate::commands::tool_file_changes(
            &core,
            node.clone(),
            concurrent_session.into(),
            concurrent_request.clone(),
            concurrent_doc.clone(),
            None,
        )
    }))
    .await;
    for result in results {
        assert_eq!(result.unwrap(), Some(concurrent_expected.clone()));
    }

    // Unavailable scope reports retryable ProjectionPending.
    let unavailable = crate::commands::tool_file_changes(
        &core,
        node.clone(),
        "not-received-session".into(),
        "not-received-request".into(),
        "physical-tool".into(),
        None,
    )
    .await
    .unwrap_err();
    let classified = unavailable
        .downcast_ref::<gents_store::client_error::BridgeError>()
        .unwrap();
    assert_eq!(
        classified.code,
        gents_store::client_error::BridgeErrorCode::ProjectionPending
    );
    assert!(classified.retryable);

    // Maximum file images cross editor host frames without truncation.
    let max_session = "file-review-maximum";
    let max_call = "maximum-file-call";
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
    let (max_doc, max_expected) = seed_review(
        &served,
        max_session,
        max_call,
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
    let stop = CancellationToken::new();
    let host = Arc::new(ClientHost::new(core.clone()));
    let (mut client, hello, task) = attach_host(host, stop.clone()).await;
    assert_eq!(hello["version"], protocol::PROTOCOL_VERSION);
    client
        .send(Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "kind": "call", "id": 1, "command": {
                    "name": "desktop_tool_file_changes", "args": {
                        "nodeDid": node, "sessionId": max_session,
                        "requestId": format!("req-{max_call}"), "toolCallDocId": max_doc,
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
        max_expected
    );
    stop.cancel();
    drop(client);
    task.await.unwrap().unwrap();
    drop(core);
    served.shutdown().await;
}

async fn seed_review(
    served: &ServedDesktop,
    session: &str,
    call: &str,
    before: FileImage,
    after: FileImage,
) -> (String, ToolFileChanges) {
    let progress = fixture_job_progress("File change", JobState::Completed, None);
    let node = served.node_did();
    let physical = seed_served_job_row(served, session, call, "completed", &progress, None).await;
    seed_admission(served, session, &physical, call, 1).await;
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
    let access = served.runtime.agent_access(&served.agent).await;
    let binding = access
        .execute(&format!(
            r#"{{ AgentToolCall(docID: "{}") {{ request_doc_id }} }}"#,
            escape_graphql_string(&physical),
        ))
        .await
        .unwrap();
    let scope = gents_protocol::session::SessionScope {
        node_did: node.into(),
        session_id: session.into(),
        requester_did: Some(served.core.node_identity().did().into()),
    };
    access.transact_write("test.file_review.images", |txn| {
        let (expected, scope, binding) = (&expected, &scope, &binding);
        Box::pin(async move {
            let stored = gents_session::file_contents::store(txn, scope, expected).await?;
            let result = txn.execute_with_variables(
                "mutation($input: ToolFileChangeMutationInputArg!) { create_ToolFileChange(input: $input) { _docID } }",
                &serde_json::json!({"input": {
                    "tool_call_doc_id": expected.tool_call_doc_id,
                    "request_doc_id": binding["data"]["AgentToolCall"][0]["request_doc_id"],
                    "node_did": scope.node_did, "session_id": scope.session_id,
                    "requester_did": scope.requester_did, "file_changes": stored,
                }}),
            ).await?;
            let observation = gents_store::graphql::created_doc_id(&result, "ToolFileChange")?;
            let reader = scope.requester_did.as_deref().unwrap().parse()?;
            txn.add_actor_relationship("ToolFileChange", &observation, "reader", &reader).await?;
            for change in stored.changes {
                if let Some(content) = change.content_doc_id {
                    txn.add_actor_relationship("ToolFileContent", &content, "reader", &reader).await?;
                }
            }
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
    (physical, expected)
}

async fn seed_admission(
    served: &ServedDesktop,
    session_id: &str,
    row_doc_id: &str,
    call: &str,
    sequence: u32,
) {
    let node_did = served.node_did();
    let requester = served.core.node_identity().did().to_owned();
    let access = served.runtime.agent_access(&served.agent).await;
    let read = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ request_doc_id session_doc_id }} }}"#,
        escape_graphql_string(row_doc_id)
    );
    let response = access
        .transact_write("test.fixture.job_row_request", |txn| {
            let read = read.clone();
            Box::pin(async move {
                txn.execute_with_variables(&read, &serde_json::json!({}))
                    .await
            })
        })
        .await
        .expect("read job row request");
    let request_doc_id = response
        .pointer("/data/AgentToolCall/0/request_doc_id")
        .and_then(|value| value.as_str())
        .expect("job row request_doc_id")
        .to_owned();
    let session_doc = response["data"]["AgentToolCall"][0]["session_doc_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = format!(
        r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ deadline_at: "2126-06-01T10:00:00Z", message_sequence: {sequence} }}) {{ _docID }} }}"#,
        escape_graphql_string(row_doc_id)
    );
    access
        .transact_write("test.fixture.job_row_deadline", |txn| {
            let deadline = deadline.clone();
            Box::pin(async move { txn.execute(&deadline).await })
        })
        .await
        .expect("seed job row deadline");
    let arguments = "{}";
    let generation = format!("cli-job-fixture:{request_doc_id}");
    let created_at = "2026-06-01T10:00:00Z";
    let segment = OutputSegment {
        node_did: node_did.into(),
        requester_did: Some(requester.clone()),
        session_id: session_id.into(),
        request_doc_id: request_doc_id.clone(),
        source: OutputSource::ProviderTurn {
            scope: CaptureScope {
                kind: CaptureScopeKind::Inference,
                seq: 1,
            },
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: arguments.len() as u32,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: call.into(),
                    call_id: Some(call.into()),
                    name: "workflow".into(),
                },
            }),
        }],
        stream_bytes_after: None,
        payload: arguments.into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![arguments.len() as u64],
        }),
        created_at: created_at.into(),
    };
    let variables = output_segment_create_variables(&segment).expect("segment variables");
    let response = access
        .transact_write("test.fixture.job_admission_segment", |txn| {
            let variables = variables.clone();
            Box::pin(async move {
                txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION, &variables)
                    .await
            })
        })
        .await
        .expect("seed admission segment");
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentOutputSegment")
            .expect("segment doc id");
    access
        .add_resource_relationship(
            "AgentOutputSegment",
            &close_doc_id,
            "parent",
            "sessions",
            &session_doc,
        )
        .await
        .unwrap();
    let message = TranscriptMessage {
        native_request_id: None,
        message_key: gents_session::session::sequence_message_key(
            node_did,
            session_id,
            Some(requester.as_str()),
            sequence,
        ),
        session_id: session_id.into(),
        node_did: node_did.into(),
        requester_did: Some(requester.clone()),
        request_doc_id: Some(request_doc_id),
        publication: MessagePublication::RequestExecution {
            execution_generation: generation,
        },
        outcome: OutputOutcome::Complete,
        sequence,
        role: MessageRole::Assistant,
        native_id: None,
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: row_doc_id.into(),
            id: call.into(),
            call_id: Some(call.into()),
            name: "workflow".into(),
            arguments: PayloadRef {
                close_doc_id,
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
        created_at: created_at.into(),
    };
    let variables = transcript_message_create_variables(&message).expect("message variables");
    let response = access
        .transact_write("test.fixture.job_admission_header", |txn| {
            let variables = variables.clone();
            Box::pin(async move {
                txn.execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &variables)
                    .await
            })
        })
        .await
        .expect("seed admission header");
    let header = gents_store::graphql::created_doc_id(&response, "AgentMessage").unwrap();
    access
        .add_resource_relationship("AgentMessage", &header, "parent", "sessions", &session_doc)
        .await
        .unwrap();
}
