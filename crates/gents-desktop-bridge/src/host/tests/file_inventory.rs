use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents::config_client::ConfigAccess;
use gents::NodeIdentity;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_protocol::file_image_hydration::FileImageScope;
use gents_protocol::file_inventory::{
    FileChangeMetadata, FileImageMetadata, FileInventoryAdmission, FileInventoryBinding,
    FileInventoryCoverage, FileInventoryCursor, FileInventoryEntry, FileInventoryObservation,
    FileInventoryPage, FileInventoryPageDemand, FileInventoryPageReceipt, FileInventoryReply,
    FileInventoryViewCursor, FILE_INVENTORY_OPERATOR_PATH, FILE_INVENTORY_RECEIPT_VERSION,
};
use serde_json::{json, Value};
use tokio::io::DuplexStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tokio_util::sync::CancellationToken;
use wiremock::{
    matchers::{body_json, header_exists, method, path},
    Mock, MockServer, ResponseTemplate,
};

use super::{protocol, ClientHost};

type Connection = Framed<DuplexStream, LengthDelimitedCodec>;

async fn attach(
    core: Arc<ClientCore>,
) -> Result<(Connection, tokio::task::JoinHandle<Result<()>>)> {
    let host = Arc::new(ClientHost::new(core));
    let (client, server) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(host.serve(server, CancellationToken::new()));
    let mut client = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(client);
    client
        .send(Bytes::from(serde_json::to_vec(&json!({
            "kind":"hello", "version":protocol::PROTOCOL_VERSION,
        }))?))
        .await?;
    let hello: Value = serde_json::from_slice(&client.next().await.context("hello missing")??)?;
    assert!(hello["commands"]
        .as_array()
        .unwrap()
        .contains(&json!("desktop_session_file_inventory")));
    Ok((client, task))
}

async fn call(client: &mut Connection, node: &str, id: u64, action: Value) -> Result<Value> {
    client
        .send(Bytes::from(serde_json::to_vec(&json!({
            "kind":"call", "id":id,
            "command":{"name":"desktop_session_file_inventory", "args":{
                "nodeDid":node, "sessionId":"inventory-bridge", "action":action,
            }},
        }))?))
        .await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let reply: Value =
                serde_json::from_slice(&client.next().await.context("bridge closed")??)?;
            if reply["kind"] == "response" && reply["id"] == id {
                return Ok(reply);
            }
        }
    })
    .await?
}

fn observation(reply: Value) -> Result<FileInventoryObservation> {
    assert!(reply["error"].is_null(), "{reply}");
    Ok(serde_json::from_value(reply["result"].clone())?)
}

async fn page_reply(
    source: &dyn NodeIdentity,
    demand: &FileInventoryPageDemand,
    entries: &[FileInventoryEntry],
    binding: &FileInventoryBinding,
) -> Result<FileInventoryReply> {
    let offset = demand.cursor.as_ref().map_or(0, |cursor| cursor.offset);
    let end = (offset as usize + 64).min(entries.len());
    let mut receipt = FileInventoryPageReceipt {
        version: FILE_INVENTORY_RECEIPT_VERSION,
        request_key: demand.derive_request_key()?,
        signer_did: source.did().into(),
        page: FileInventoryPage {
            binding: binding.clone(),
            offset,
            entries: entries[offset as usize..end].to_vec(),
            next: (end < entries.len()).then(|| FileInventoryCursor {
                binding: binding.clone(),
                offset: end as u64,
            }),
        },
        signature: Vec::new(),
    };
    receipt.signature = source.sign(&receipt.signing_payload()?).await?;
    Ok(FileInventoryReply::Page(receipt))
}

async fn mount_reply(
    server: &MockServer,
    demand: &FileInventoryPageDemand,
    reply: &FileInventoryReply,
) {
    Mock::given(method("POST"))
        .and(path(FILE_INVENTORY_OPERATOR_PATH))
        .and(header_exists("authorization"))
        .and(body_json(demand))
        .respond_with(ResponseTemplate::new(200).set_body_json(reply))
        .expect(1)
        .mount(server)
        .await;
}

async fn metadata_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == FILE_INVENTORY_OPERATOR_PATH)
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inventory_pages_refresh_and_reload_through_the_live_bridge() -> Result<()> {
    let root = tempfile::tempdir()?;
    let key = root.path().join("runtime.key");
    let source = tokio::task::spawn_blocking(move || gents::KeyIdentity::load_or_create(key, None))
        .await??;
    let server = MockServer::start().await;
    let endpoint = format!("{}/api/v0/graphql", server.uri());
    tokio::fs::write(
        root.path().join("runtime.json"),
        json!({
            "node_name":"Inventory runtime", "node_did":source.did(), "graphql":endpoint,
            "p2p_transport":"iroh",
        })
        .to_string(),
    )
    .await?;
    let paths = DesktopPaths::from_root(root.path().join("client"));
    let options = ClientCoreOptions {
        operator_identity_interaction:
            gents_desktop_core::local_runtime::OperatorIdentityInteraction::Prevent,
        ..ClientCoreOptions::local_only()
    };
    let core =
        Arc::new(ClientCore::start_with_paths_and_options(paths.clone(), options.clone()).await?);
    core.persist_local_standard_peer(
        "Inventory runtime",
        "endpoint:bridge-inventory",
        source.did(),
        &endpoint,
        root.path().to_str().unwrap(),
    )
    .await?;
    ConfigAccess::transact_local_write(&core.node_arc(), None, "test.inventory.bridge_session", |txn| {
        let did = source.did();
        Box::pin(async move { txn.execute_with_variables(
            "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
            &json!({"input": {"node_did":did, "session_id":"inventory-bridge", "requester_did":did,
                "agent_id":"fixture", "created_at":"2026-10-24T00:00:00Z"}})).await })
    }).await?;
    let (mut client, task) = attach(core.clone()).await?;
    let initial = observation(call(&mut client, source.did(), 1, json!({"kind":"read"})).await?)?;
    assert!(initial.current.is_none() && initial.previous.is_none() && initial.refusal.is_none());
    assert!(server.received_requests().await.unwrap().is_empty());
    let admission: FileInventoryAdmission = initial.admission.clone();
    let entries: Vec<_> = (0..65)
        .map(|index| FileInventoryEntry {
            tool_call_doc_id: "tool-document".into(),
            request_doc_id: "request-document".into(),
            scope: FileImageScope {
                requester_did: source.did().into(),
                node_did: source.did().into(),
                session_id: "inventory-bridge".into(),
            },
            projection_doc_id: "projection-document".into(),
            observation_doc_id: "capture-document".into(),
            change: FileChangeMetadata {
                root: "/fixture".into(),
                path: format!("file-{index:02}.rs"),
                before: FileImageMetadata::Missing {},
                after: FileImageMetadata::File {
                    content_hash: format!("fixture-hash-{index}"),
                },
            },
        })
        .collect();
    let binding = FileInventoryBinding {
        admission: admission.clone(),
        digest: "fixture-snapshot".into(),
        coverage: FileInventoryCoverage {
            entries: 65,
            pending: 0,
            unknown: 0,
        },
    };
    let mut demand = FileInventoryPageDemand {
        admission,
        attempt_id: initial.attempt_id.clone(),
        cursor: None,
    };
    mount_reply(
        &server,
        &demand,
        &page_reply(&source, &demand, &entries, &binding).await?,
    )
    .await;
    let partial =
        observation(call(&mut client, source.did(), 2, json!({"kind":"advance"})).await?)?;
    let page = partial.current.as_ref().unwrap();
    assert_eq!(page.entries, entries[..64]);
    assert_eq!(page.received_entries, 64);
    assert!(!page.complete && !page.known_inventory && page.next.is_none());
    demand.cursor = Some(FileInventoryCursor {
        binding: binding.clone(),
        offset: 64,
    });
    mount_reply(
        &server,
        &demand,
        &page_reply(&source, &demand, &entries, &binding).await?,
    )
    .await;
    let complete =
        observation(call(&mut client, source.did(), 3, json!({"kind":"advance"})).await?)?;
    let page = complete.current.as_ref().unwrap();
    assert_eq!(page.entries, entries[..64]);
    assert_eq!(page.received_entries, 65);
    assert!(page.complete && page.known_inventory);
    let cursor = page.next.as_ref().unwrap().clone();
    let last = observation(
        call(
            &mut client,
            source.did(),
            4,
            json!({"kind":"read", "cursor":cursor}),
        )
        .await?,
    )?;
    let page = last.current.as_ref().unwrap();
    assert_eq!(page.entries, entries[64..]);
    assert_eq!(page.offset, 64);
    assert!(page.next.is_none() && page.complete);
    let unchanged =
        observation(call(&mut client, source.did(), 5, json!({"kind":"advance"})).await?)?;
    assert_eq!(unchanged, complete);
    assert_eq!(metadata_requests(&server).await, 2);
    let stale = FileInventoryViewCursor {
        snapshot_digest: "changed".into(),
        ..cursor.clone()
    };
    assert!(!call(
        &mut client,
        source.did(),
        6,
        json!({"kind":"read", "cursor":stale})
    )
    .await?["error"]
        .is_null());
    let refresh_id = uuid::Uuid::new_v4().to_string();
    let refresh = observation(
        call(
            &mut client,
            source.did(),
            7,
            json!({"kind":"refresh", "attemptId":refresh_id}),
        )
        .await?,
    )?;
    assert!(refresh.current.is_none());
    assert_eq!(
        refresh.previous.as_ref().unwrap(),
        complete.current.as_ref().unwrap()
    );
    let previous = observation(
        call(
            &mut client,
            source.did(),
            8,
            json!({"kind":"read", "cursor":cursor}),
        )
        .await?,
    )?;
    assert_eq!(previous.previous.as_ref().unwrap().entries, entries[64..]);
    demand.attempt_id = refresh.attempt_id.clone();
    demand.cursor = None;
    let refused = FileInventoryReply::Refused(
        gents::file_inventory::sign_refusal(
            &source,
            &demand,
            gents_protocol::file_inventory::FileInventoryRefusalReason::Unavailable,
        )
        .await?,
    );
    mount_reply(&server, &demand, &refused).await;
    let retained =
        observation(call(&mut client, source.did(), 9, json!({"kind":"advance"})).await?)?;
    assert!(retained.current.is_none());
    assert_eq!(
        retained.refusal,
        Some(gents_protocol::file_inventory::FileInventoryRefusalReason::Unavailable)
    );
    assert_eq!(retained.previous, complete.current);
    server.verify().await;
    drop(client);
    task.await??;
    core.shutdown().await?;
    drop(core);
    let core = Arc::new(ClientCore::start_with_paths_and_options(paths, options).await?);
    let (mut client, task) = attach(core.clone()).await?;
    let reloaded = observation(call(&mut client, source.did(), 10, json!({"kind":"read"})).await?)?;
    assert_eq!(reloaded, retained);
    let unchanged =
        observation(call(&mut client, source.did(), 11, json!({"kind":"advance"})).await?)?;
    assert_eq!(unchanged, retained);
    assert_eq!(metadata_requests(&server).await, 3);
    server.verify().await;
    let stored = ConfigAccess::local(core.node_arc()).execute(
        "{ AgentRequest { _docID } FileInventoryRequest { _docID } ToolFileInventory { _docID } ToolFileChange { _docID } }",
    ).await?;
    for collection in [
        "AgentRequest",
        "FileInventoryRequest",
        "ToolFileInventory",
        "ToolFileChange",
    ] {
        assert_eq!(
            stored["data"][collection],
            json!([]),
            "{collection}: {stored}"
        );
    }
    drop(client);
    task.await??;
    core.shutdown().await?;
    Ok(())
}
