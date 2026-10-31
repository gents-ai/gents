use super::*;
use gents::NodeIdentity;
use gents_protocol::file_changes::{FileChangeImages, FileImage, FilePermissions};
use gents_protocol::file_image_hydration::FileImageScope;
use gents_protocol::file_inventory::{
    FileInventoryAdmission, FileInventoryBinding, FileInventoryCoverage, FileInventorySnapshotView,
    FileInventoryViewCursor,
};

fn fixture() -> (
    FileInventoryObservation,
    FileInventoryImageReference,
    ToolFileChanges,
) {
    let change = FileChangeImages {
        root: "/fixture".into(),
        path: "src/main.rs".into(),
        before: FileImage::Missing,
        after: FileImage::File {
            content_hash: "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .into(),
            data: String::new(),
            permissions: FilePermissions {
                readonly: false,
                unix_mode: Some(0o644),
            },
            links: Some(1),
        },
    };
    let entry = FileInventoryEntry {
        request_coverage: None,
        order: None,
        tool_call_doc_id: "tool-physical".into(),
        request_doc_id: "request-physical".into(),
        scope: FileImageScope {
            requester_did: "origin-requester".into(),
            node_did: "origin-node".into(),
            session_id: "origin-session".into(),
        },
        projection_doc_id: "metadata-physical".into(),
        observation_doc_id: "image-physical".into(),
        change: FileChangeMetadata::from(&change),
    };
    let admission = FileInventoryAdmission {
        request: HydrationRequest {
            request_key: "target-request".into(),
            peer_id: "client-peer".into(),
            requester_did: "target-requester".into(),
            node_did: "target-node".into(),
            session_id: "target-session".into(),
        },
        session_doc_id: "target-physical".into(),
    };
    let reference = FileInventoryImageReference {
        cursor: FileInventoryViewCursor {
            attempt_id: "attempt-1".into(),
            snapshot_digest: "signed-snapshot".into(),
            offset: 0,
        },
        entry_index: 0,
    };
    let observation = FileInventoryObservation {
        admission: admission.clone(),
        attempt_id: "attempt-1".into(),
        current: Some(FileInventorySnapshotView {
            attempt_id: "attempt-1".into(),
            binding: FileInventoryBinding {
                admission,
                digest: "signed-snapshot".into(),
                coverage: FileInventoryCoverage {
                    entries: 1,
                    pending: 0,
                    unknown: 0,
                },
            },
            received_entries: 1,
            complete: true,
            known_inventory: true,
            offset: 0,
            entries: vec![entry],
            next: None,
        }),
        previous: None,
        refusal: None,
    };
    (
        observation,
        reference,
        ToolFileChanges {
            version: FILE_CHANGES_VERSION,
            tool_call_doc_id: "tool-physical".into(),
            changes: vec![change],
        },
    )
}

#[test]
fn selected_record_keeps_its_origin_and_exact_received_coordinate() {
    let (mut observation, reference, _) = fixture();
    let expected = observation.current.as_ref().unwrap().entries[0].clone();
    assert_eq!(selected_entry(&observation, &reference).unwrap(), &expected);
    observation.attempt_id = "refresh-attempt".into();
    observation.previous = observation.current.take();
    assert_eq!(selected_entry(&observation, &reference).unwrap(), &expected);
    for changed in [
        FileInventoryImageReference {
            entry_index: 1,
            ..reference.clone()
        },
        FileInventoryImageReference {
            cursor: FileInventoryViewCursor {
                attempt_id: "other".into(),
                ..reference.cursor.clone()
            },
            ..reference.clone()
        },
        FileInventoryImageReference {
            cursor: FileInventoryViewCursor {
                snapshot_digest: "other".into(),
                ..reference.cursor.clone()
            },
            ..reference.clone()
        },
        FileInventoryImageReference {
            cursor: FileInventoryViewCursor {
                offset: 64,
                ..reference.cursor.clone()
            },
            ..reference.clone()
        },
    ] {
        assert!(selected_entry(&observation, &changed).is_err());
    }
}

#[test]
fn image_binding_rejects_changed_observation_tool_path_and_hash() {
    let (observation, reference, changes) = fixture();
    let entry = selected_entry(&observation, &reference).unwrap();
    assert!(match_observation(&reference, entry.clone(), "other-image", changes.clone()).is_err());
    let mut wrong = changes.clone();
    wrong.tool_call_doc_id = "other-tool".into();
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.version += 1;
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.changes[0].path = "other-file".into();
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.changes[0].after = FileImage::Missing;
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
    let mut wrong = changes.clone();
    wrong.changes.push(wrong.changes[0].clone());
    assert!(match_observation(&reference, entry.clone(), "image-physical", wrong).is_err());
}

#[test]
fn a_single_recorded_file_is_returned_from_a_capture_larger_than_one_metadata_page() {
    let (observation, reference, mut changes) = fixture();
    let entry = selected_entry(&observation, &reference).unwrap();
    let expected = changes.changes[0].clone();
    for index in 0..64 {
        let mut other = expected.clone();
        other.path = format!("other-{index}.rs");
        changes.changes.push(other);
    }
    let image = match_observation(&reference, entry.clone(), "image-physical", changes).unwrap();
    assert_eq!(image.reference, reference);
    assert_eq!(image.entry, *entry);
    assert_eq!(image.change, expected);
}

async fn insert(
    node: &std::sync::Arc<gents::defra_node::EmbeddedNode>,
    collection: &'static str,
    input: serde_json::Value,
) -> Result<String> {
    let response = ConfigAccess::transact_local_write(node, None, "test.inventory.image_source", |txn| {
        let input = &input;
        Box::pin(async move { txn.execute_with_variables(&format!(
            "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}",
        ), &serde_json::json!({"input":input})).await })
    }).await?;
    Ok(gents::store::graphql::created_doc_id(
        &response, collection,
    )?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recorded_origin_review_retains_metadata_without_a_route_but_does_not_grant_images(
) -> Result<()> {
    use gents::defra_node::{EmbeddedNode, HttpConfig};
    use gents_protocol::file_inventory::{
        FileInventoryPage, FileInventoryPageDemand, FileInventoryPageReceipt, FileInventoryReply,
        FILE_INVENTORY_RECEIPT_VERSION,
    };
    use gents_protocol::output::*;
    use serde_json::json;
    use std::sync::Arc;
    let root = tempfile::tempdir()?;
    let key = root.path().join("source.key");
    let source = tokio::task::spawn_blocking(move || gents::KeyIdentity::load_or_create(key, None))
        .await??;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(source.did())
            .with_node_acp_enabled()
            .with_http(HttpConfig::with_listener(listener.into_std()?)?)
            .build()
            .await?,
    );
    gents::ensure_runtime_schemas(&node).await?;
    let now = "2026-10-24T00:00:00Z";
    let scope = json!({"node_did":source.did(), "requester_did":source.did(), "session_id":"original-chat"});
    let mut original_session = scope.clone();
    original_session.as_object_mut().unwrap().extend(
        json!({
            "agent_id":"fixture", "created_at":now,
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    insert(&node, "AgentSession", original_session).await?;
    let request = insert(
        &node,
        "AgentRequest",
        json!({
            "purpose":"normal", "request_id":"origin-request", "node_did":source.did(),
            "requester_did":source.did(), "session_id":"original-chat", "agent_id":"fixture",
            "content":"recorded edit", "lifecycle_state":"completed", "backend_id":"",
            "created_at":now, "retry_count":0, "execution_generation":"fixture-generation",
        }),
    )
    .await?;
    let mut tool = scope.clone();
    tool.as_object_mut().unwrap().extend(
        json!({
            "tool_call_key":"fixture-call", "tool_call_id":"fixture-call", "tool_name":"write_file",
            "request_id":"origin-request", "request_doc_id":request, "message_sequence":1,
            "lifecycle_state":"completed", "await_mode":"foreground", "started_at":now,
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let tool = insert(&node, "AgentToolCall", tool).await?;
    let segment = OutputSegment {
        node_did: source.did().into(),
        requester_did: Some(source.did().into()),
        session_id: "original-chat".into(),
        request_doc_id: request.clone(),
        source: OutputSource::ProviderTurn {
            scope: "inference.1".parse()?,
            turn_index: 0,
            attempt: 0,
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: "fixture-generation".into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: 2,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolArguments {
                    id: "fixture-call".into(),
                    call_id: Some("provider-call".into()),
                    name: "write_file".into(),
                },
            }),
        }],
        payload: "{}".into(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![2],
        }),
        created_at: now.into(),
    };
    let close = insert(&node, "AgentOutputSegment", serde_json::to_value(segment)?).await?;
    let header = TranscriptMessage {
        message_key: "fixture-header".into(),
        session_id: "original-chat".into(),
        node_did: source.did().into(),
        requester_did: Some(source.did().into()),
        request_doc_id: Some(request.clone()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "fixture-generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 1,
        role: MessageRole::Assistant,
        native_id: Some("provider-message".into()),
        blocks: vec![MessageBlock::ToolCall {
            tool_call_doc_id: tool.clone(),
            id: "fixture-call".into(),
            call_id: Some("provider-call".into()),
            name: "write_file".into(),
            arguments: PayloadRef {
                close_doc_id: close,
                stream: 0,
            },
            signature: None,
            additional_params: None,
        }],
        created_at: now.into(),
    };
    insert(&node, "AgentMessage", serde_json::to_value(header)?).await?;
    let (_, _, mut changes) = fixture();
    changes.tool_call_doc_id = tool.clone();
    let expected = changes.changes[0].clone();
    let mut image_input = scope;
    image_input.as_object_mut().unwrap().extend(
        json!({
            "tool_call_doc_id":tool, "request_doc_id":request, "file_changes":changes,
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    let image = insert(&node, "ToolFileChange", image_input).await?;
    tokio::fs::write(root.path().join("runtime.json"), json!({
        "node_name":"Origin runtime", "node_did":source.did(), "graphql":endpoint, "p2p_transport":"iroh",
    }).to_string()).await?;
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            crate::client::DesktopPaths::from_root(root.path().join("client")),
            crate::client::ClientCoreOptions::local_only(),
        )
        .await?,
    );
    core.persist_local_standard_peer(
        "Origin runtime",
        "endpoint:origin-image",
        source.did(),
        &endpoint,
        root.path().to_str().unwrap(),
    )
    .await?;
    let entry = FileInventoryEntry {
        request_coverage: None,
        order: None,
        tool_call_doc_id: tool,
        request_doc_id: request,
        scope: FileImageScope {
            requester_did: source.did().into(),
            node_did: source.did().into(),
            session_id: "original-chat".into(),
        },
        projection_doc_id: "signed-metadata-reference".into(),
        observation_doc_id: image,
        change: FileChangeMetadata::from(&expected),
    };
    let target_session = insert(
        &node,
        "AgentSession",
        json!({
            "node_did":source.did(), "requester_did":source.did(), "session_id":"target-chat",
            "agent_id":"fixture", "created_at":now,
        }),
    )
    .await?;
    let admission = FileInventoryAdmission {
        request: HydrationRequest {
            request_key: format!("{}:target-chat", core.local_peer_id()),
            peer_id: core.local_peer_id().into(),
            requester_did: source.did().into(),
            node_did: source.did().into(),
            session_id: "target-chat".into(),
        },
        session_doc_id: target_session,
    };
    let attempt = uuid::Uuid::new_v4().to_string();
    let demand = FileInventoryPageDemand {
        admission: admission.clone(),
        attempt_id: attempt.clone(),
        cursor: None,
    };
    gents::file_inventory::begin_client_progress(
        &core.node_arc(),
        &admission,
        &attempt,
        core.node_identity(),
    )
    .await?;
    let mut receipt = FileInventoryPageReceipt {
        version: FILE_INVENTORY_RECEIPT_VERSION,
        request_key: demand.derive_request_key()?,
        signer_did: source.did().into(),
        page: FileInventoryPage {
            binding: FileInventoryBinding {
                admission,
                digest: "signed-origin-snapshot".into(),
                coverage: FileInventoryCoverage {
                    entries: 1,
                    pending: 0,
                    unknown: 0,
                },
            },
            offset: 0,
            entries: vec![entry.clone()],
            next: None,
        },
        signature: Vec::new(),
    };
    receipt.signature = source.sign(&receipt.signing_payload()?).await?;
    gents::file_inventory::receive_client_reply(
        &core.node_arc(),
        &demand,
        &FileInventoryReply::Page(receipt),
        core.node_identity(),
    )
    .await?;
    let reference = FileInventoryImageReference {
        cursor: FileInventoryViewCursor {
            attempt_id: attempt,
            snapshot_digest: "signed-origin-snapshot".into(),
            offset: 0,
        },
        entry_index: 0,
    };
    let refresh = uuid::Uuid::new_v4().to_string();
    let pending = core
        .session_file_inventory(
            source.did(),
            "target-chat",
            FileInventoryReadAction::Refresh {
                attempt_id: refresh.clone(),
            },
        )
        .await?
        .unwrap();
    assert_eq!(pending.attempt_id, refresh);
    assert_eq!(
        pending.previous.as_ref().unwrap().entries,
        vec![entry.clone()]
    );
    let observed = core
        .inventory_file_change(source.did(), "target-chat", &reference, None)
        .await?
        .unwrap();
    assert_eq!(observed.entry, entry);
    assert_eq!(observed.change, expected);
    assert_eq!(observed.reference, reference);
    let mut record = core
        .sync_state
        .records()
        .iter()
        .find(|record| record.node_did == source.did())
        .unwrap()
        .clone();
    record.local_node_home = None;
    core.sync_state.upsert_for_test(record.clone()).await?;
    core.remove_peer(&record.peer_id).await?;
    assert!(core.sync_state.records().is_empty());
    let retained = core
        .session_file_inventory(
            source.did(),
            "target-chat",
            FileInventoryReadAction::Read {
                cursor: Some(reference.cursor.clone()),
            },
        )
        .await?
        .unwrap();
    assert_eq!(
        retained.previous.as_ref().unwrap().entries,
        vec![entry.clone()]
    );
    let offline_refresh = uuid::Uuid::new_v4().to_string();
    let pending = core
        .session_file_inventory(
            source.did(),
            "target-chat",
            FileInventoryReadAction::Refresh {
                attempt_id: offline_refresh.clone(),
            },
        )
        .await?
        .unwrap();
    assert_eq!(pending.attempt_id, offline_refresh);
    assert_eq!(
        pending.previous.as_ref().unwrap().entries,
        vec![entry.clone()]
    );
    assert!(core
        .session_file_inventory(
            source.did(),
            "target-chat",
            FileInventoryReadAction::Advance {}
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("Reconnect"));
    assert!(core
        .inventory_file_change(source.did(), "target-chat", &reference, None)
        .await
        .is_err());
    assert!(core
        .session_file_inventory(
            source.did(),
            "another-chat",
            FileInventoryReadAction::default()
        )
        .await
        .is_err());
    insert(
        &core.node_arc(),
        "AgentSession",
        json!({
            "node_did":source.did(), "requester_did":source.did(), "session_id":"target-chat",
            "agent_id":"fixture", "created_at":now,
        }),
    )
    .await?;
    assert!(core
        .session_file_inventory(
            source.did(),
            "target-chat",
            FileInventoryReadAction::default()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("different physical session"));
    let local = ConfigAccess::local(core.node_arc()).execute(
        "{ AgentRequest { _docID } AgentMessage { _docID } FileImageHydrationRequest { _docID } }",
    ).await?;
    for collection in ["AgentRequest", "AgentMessage", "FileImageHydrationRequest"] {
        assert_eq!(local["data"][collection], json!([]));
    }
    core.shutdown().await?;
    node.shutdown().await;
    Ok(())
}

#[test]
fn independent_reader_image_demand_binds_received_page_without_impersonating_author() {
    let (mut observation, reference, _) = fixture();
    let view = observation.current.as_mut().unwrap();
    view.entries[0].scope.node_did = view.binding.admission.request.node_did.clone();
    let demand =
        selected_image_demand(&observation, &reference, "target-requester", "client-peer").unwrap();
    assert_eq!(demand.admission.requester_did, "target-requester");
    assert_eq!(demand.admission.session_id, "target-session");
    assert_eq!(
        selected_entry(&observation, &reference)
            .unwrap()
            .scope
            .requester_did,
        "origin-requester"
    );
    let selected = demand.inventory_reference.as_ref().unwrap();
    assert_eq!(selected.page.admission.session_doc_id, "target-physical");
    assert_eq!(selected.snapshot_digest, "signed-snapshot");
    assert_eq!(selected.page.attempt_id, reference.cursor.attempt_id);
    assert!(selected.page.cursor.is_none());
    assert_eq!(demand.request_doc_id, "request-physical");
    demand.validate_reference().unwrap();
    assert!(
        selected_image_demand(&observation, &reference, "wrong-reader", "client-peer").is_err()
    );
    assert!(
        selected_image_demand(&observation, &reference, "target-requester", "wrong-peer").is_err()
    );
    let later_view = observation.current.as_mut().unwrap();
    later_view.offset = 64;
    later_view.binding.coverage.entries = 65;
    later_view.received_entries = 65;
    let later = FileInventoryImageReference {
        cursor: FileInventoryViewCursor {
            offset: 64,
            ..reference.cursor.clone()
        },
        ..reference.clone()
    };
    let demand =
        selected_image_demand(&observation, &later, "target-requester", "client-peer").unwrap();
    let selected = demand.inventory_reference.as_ref().unwrap();
    assert_eq!(selected.page.cursor.as_ref().unwrap().offset, 64);
    demand.validate_reference().unwrap();
    observation.previous = observation.current.take();
    assert_eq!(
        selected_image_demand(&observation, &later, "target-requester", "client-peer").unwrap(),
        demand
    );
}

#[test]
fn canonical_author_keeps_existing_image_receipt_binding() {
    let (observation, reference, _) = fixture();
    let demand =
        selected_image_demand(&observation, &reference, "origin-requester", "client-peer").unwrap();
    assert!(demand.inventory_reference.is_none());
    assert_eq!(
        demand.receipt_version(),
        gents_protocol::file_image_hydration::FILE_IMAGE_RECEIPT_VERSION
    );
    assert_eq!(demand.admission.requester_did, "origin-requester");
    assert_eq!(demand.admission.session_id, "origin-session");
}
