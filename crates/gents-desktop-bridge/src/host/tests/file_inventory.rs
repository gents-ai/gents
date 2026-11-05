use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents::NodeIdentity;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_protocol::file_inventory::{
    DirectFileInventoryRequest, FileInventoryAdmission, FileInventoryObservation,
    FileInventoryReadAction, FILE_INVENTORY_OPERATOR_PATH,
};
use gents_protocol::session_hydration::HydrationRequest;
use identity::Identity;
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
                "nodeDid":node, "sessionId":"inventory-bridge", "action":action
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inventory_requests_go_to_the_runtime_as_the_desktop_client() -> Result<()> {
    let root = tempfile::tempdir()?;
    let key = root.path().join("runtime.key");
    let runtime =
        tokio::task::spawn_blocking(move || gents::KeyIdentity::load_or_create(key, None))
            .await??;
    let server = MockServer::start().await;
    let endpoint = format!("{}/api/v0/graphql", server.uri());
    let paths = DesktopPaths::from_root(root.path().join("client"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?,
    );
    core.persist_local_standard_peer(
        "Inventory runtime",
        "endpoint:bridge-inventory",
        runtime.did(),
        &endpoint,
        root.path().to_str().unwrap(),
    )
    .await?;
    let request = DirectFileInventoryRequest {
        node_did: runtime.did().into(),
        session_id: "inventory-bridge".into(),
        peer_id: core.local_peer_id().into(),
        action: FileInventoryReadAction::Read { cursor: None },
    };
    let observation = FileInventoryObservation {
        admission: FileInventoryAdmission {
            request: HydrationRequest {
                request_key: "request".into(),
                peer_id: core.local_peer_id().into(),
                requester_did: core.node_identity().did().into(),
                node_did: runtime.did().into(),
                session_id: "inventory-bridge".into(),
            },
            session_doc_id: "session-document".into(),
        },
        attempt_id: "attempt".into(),
        current: None,
        previous: None,
        refusal: None,
    };
    Mock::given(method("POST"))
        .and(path(FILE_INVENTORY_OPERATOR_PATH))
        .and(header_exists("authorization"))
        .and(body_json(&request))
        .respond_with(ResponseTemplate::new(200).set_body_json(&observation))
        .expect(1)
        .mount(&server)
        .await;
    let (mut client, task) = attach(core.clone()).await?;
    let reply = call(&mut client, runtime.did(), 1, json!({"kind":"read"})).await?;
    assert!(reply["error"].is_null(), "{reply}");
    assert_eq!(
        serde_json::from_value::<FileInventoryObservation>(reply["result"].clone())?,
        observation
    );
    server.verify().await;
    let requests = server.received_requests().await.unwrap();
    let request = requests
        .iter()
        .find(|request| request.url.path() == FILE_INVENTORY_OPERATOR_PATH)
        .unwrap();
    let authorization = request.headers.get("authorization").unwrap().to_str()?;
    let token = identity::from_token(
        authorization
            .strip_prefix("Bearer ")
            .context("missing bearer")?
            .as_bytes(),
    )?;
    let audience = reqwest::Url::parse(&server.uri())?.authority().to_owned();
    identity::verify_auth_token(&token, &audience)?;
    assert_eq!(token.did()?.as_str(), core.node_identity().did());
    assert_ne!(token.did()?.as_str(), runtime.did());
    drop(client);
    task.await??;
    core.shutdown().await?;
    Ok(())
}
