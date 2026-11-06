use super::*;
use crate::client::peer_directory::{PeerDirectory, PeerRecord};
use crate::client::store::ClientStore;
use gents::store::config_client::{GraphqlEndpoint, TransactionFailure};
use gents::store::identity::{KeyIdentity, NodeIdentity};

#[test]
fn observer_stops_only_for_an_explicit_authorization_refusal() {
    let denial = gents::store::config_client::ConfigClientError::Database(
        gents_protocol::database_error::GraphqlFailure::new(
            "subscription",
            serde_json::json!([
                {"message":"opaque", "extensions":{"code":"PERMISSION_DENIED"}}
            ]),
        ),
    );
    assert!(anyhow::Error::new(denial).permission_denied());
    assert!(!anyhow::anyhow!("not found or unavailable").permission_denied());
}

async fn wait_for_name(store: &ObservedStore, expected: Option<&str>) {
    let mut notices = store.subscribe_changes();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = store.snapshot();
            let name = snapshot
                .nodes
                .first()
                .and_then(|node| node.display_name.as_deref());
            if name == expected {
                break;
            }
            notices.changed().await.expect("observer alive");
        }
    })
    .await
    .expect("authenticated runtime observation");
}

async fn wait_for_connection(store: &ObservedStore, node: &str, expected: RuntimeConnectionStatus) {
    let mut notices = store.subscribe_changes();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store.runtime_connection(node) == Some(expected) {
                break;
            }
            notices.changed().await.expect("observer alive");
        }
    })
    .await
    .expect("runtime connection status");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProxyMode {
    Online,
    Offline,
    /// Answers every request with the runtime's HTTP refusal.
    Denied,
}

type ProxyCommands = tokio::sync::mpsc::UnboundedSender<(ProxyMode, tokio::sync::oneshot::Sender<()>)>;

async fn read_request(client: &mut tokio::net::TcpStream) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = client.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..read]);
        let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
        let length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or_default();
        if request.len() >= end + 4 + length {
            return Ok(());
        }
    }
}

async fn interruptible_proxy(
    upstream: std::net::SocketAddr,
) -> (String, ProxyCommands, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let (commands, mut command_rx) = tokio::sync::mpsc::unbounded_channel::<(
        ProxyMode,
        tokio::sync::oneshot::Sender<()>,
    )>();
    let task = tokio::spawn(async move {
        let mut mode = ProxyMode::Online;
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some((next, done)) = command else { break };
                    mode = next;
                    if mode != ProxyMode::Online {
                        connections.abort_all();
                        while connections.join_next().await.is_some() {}
                    }
                    let _ = done.send(());
                }
                accepted = listener.accept() => {
                    let (mut client, _) = accepted.unwrap();
                    match mode {
                        ProxyMode::Online => {
                            connections.spawn(async move {
                                if let Ok(mut server) = tokio::net::TcpStream::connect(upstream).await {
                                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                                }
                            });
                        }
                        ProxyMode::Denied => {
                            connections.spawn(async move {
                                use tokio::io::AsyncWriteExt;
                                if read_request(&mut client).await.is_ok() {
                                    let _ = client
                                        .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                                        .await;
                                    let _ = client.shutdown().await;
                                }
                            });
                        }
                        ProxyMode::Offline => {}
                    }
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    (url, commands, task)
}

async fn set_proxy_mode(commands: &ProxyCommands, mode: ProxyMode) {
    let (done, completed) = tokio::sync::oneshot::channel();
    commands.send((mode, done)).unwrap();
    completed.await.unwrap();
}

async fn set_proxy_online(commands: &ProxyCommands, online: bool) {
    set_proxy_mode(
        commands,
        if online {
            ProxyMode::Online
        } else {
            ProxyMode::Offline
        },
    )
    .await;
}

/// A runtime whose Node document the client reads through `runtime_reader`.
struct ObservedRuntime {
    _directory: tempfile::TempDir,
    client: KeyIdentity,
    runtime_did: String,
    runtime: Arc<defra_node::Node>,
    access: ConfigAccess,
    physical: String,
    peers: PeerDirectory,
    proxy_commands: ProxyCommands,
    proxy_task: tokio::task::JoinHandle<()>,
}

impl ObservedRuntime {
    async fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let key_path = directory.path().join("client.key");
        let client = tokio::task::spawn_blocking(move || {
            KeyIdentity::load_or_create(key_path, None).unwrap()
        })
        .await
        .unwrap();
        let node_path = directory.path().join("runtime.key");
        let runtime_identity = tokio::task::spawn_blocking(move || {
            KeyIdentity::load_or_create(node_path, None).unwrap()
        })
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/api/v0/graphql");
        let (proxy_url, proxy_commands, proxy_task) = interruptible_proxy(address).await;
        let runtime = Arc::new(
            crate::test_support::node_builder()
                .with_node_identity_did(runtime_identity.did())
                .with_http(
                    defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap(),
                )
                .build()
                .await
                .unwrap(),
        );
        crate::client::schema::ensure_runtime_schemas(&runtime)
            .await
            .unwrap();
        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(url, runtime_identity.did())
                .with_client_signing_capability(
                    runtime_identity.client_signing_capability().unwrap(),
                )
                .unwrap(),
        );
        let owner =
            ConfigAccess::local_as(runtime.clone(), runtime_identity.did().parse().unwrap());
        let did = gents::store::graphql::escape_graphql_string(runtime_identity.did());
        let created = access
            .write(
                "test.observer.seed",
                &format!("mutation {{ create_Node(input: {{node_did: \"{did}\", display_name: \"remote\", enabled: true, created_at: \"2026-11-04T00:00:00Z\", created_by: \"user\"}}) {{ _docID }} }}"),
            )
            .await
            .unwrap();
        let physical = gents::store::graphql::created_doc_id(&created, "Node").unwrap();
        owner
            .add_actor_relationship(
                "Node",
                &physical,
                "runtime_reader",
                &client.did().parse().unwrap(),
            )
            .await
            .unwrap();
        let mut peer = PeerRecord::new("runtime", "", runtime_identity.did());
        peer.graphql = Some(proxy_url);
        let mut peers = PeerDirectory::open_writer(directory.path().join("peers.json"))
            .await
            .unwrap();
        peers.upsert(peer).await.unwrap();
        Self {
            _directory: directory,
            client,
            runtime_did: runtime_identity.did().to_owned(),
            runtime,
            access,
            physical,
            peers,
            proxy_commands,
            proxy_task,
        }
    }

    async fn stop(self) {
        drop(self.proxy_commands);
        self.proxy_task.await.unwrap();
        self.runtime.shutdown().await.unwrap();
    }
}

/// Arms every subscription and then never answers a snapshot query.
async fn stalled_runtime() -> (String, watch::Receiver<u64>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let (queries_tx, queries) = watch::channel(0_u64);
    let queries_tx = Arc::new(queries_tx);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            let queries_tx = queries_tx.clone();
            connections.spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut head = Vec::new();
                let mut buffer = [0_u8; 4096];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                    let Ok(read) = client.read(&mut buffer).await else { return };
                    if read == 0 {
                        return;
                    }
                    head.extend_from_slice(&buffer[..read]);
                }
                if String::from_utf8_lossy(&head)
                    .to_ascii_lowercase()
                    .contains("accept: text/event-stream")
                {
                    let _ = client
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n")
                        .await;
                } else {
                    queries_tx.send_modify(|count| *count += 1);
                }
                std::future::pending::<()>().await;
            });
        }
    });
    (url, queries, task)
}

#[tokio::test]
async fn authenticated_runtime_reconnect_reloads_and_resubscribes() {
    let directory = tempfile::tempdir().unwrap();
    let key_path = directory.path().join("client.key");
    let client =
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key_path, None).unwrap())
            .await
            .unwrap();
    let node_path = directory.path().join("runtime.key");
    let runtime_identity =
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(node_path, None).unwrap())
            .await
            .unwrap();
    assert_ne!(client.did(), runtime_identity.did());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let url = format!("http://{address}/api/v0/graphql");
    let (proxy_url, proxy_commands, proxy_task) = interruptible_proxy(address).await;
    let runtime = Arc::new(
        crate::test_support::node_builder()
            .with_node_identity_did(runtime_identity.did())
            .with_http(defra_node::HttpConfig::with_listener(listener.into_std().unwrap()).unwrap())
            .build()
            .await
            .unwrap(),
    );
    let desktop = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&runtime)
        .await
        .unwrap();
    crate::client::schema::ensure_runtime_schemas(&desktop)
        .await
        .unwrap();
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(url.clone(), runtime_identity.did())
            .with_client_signing_capability(runtime_identity.client_signing_capability().unwrap())
            .unwrap(),
    );
    let reader_access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(url.clone(), client.did())
            .with_client_signing_capability(client.client_signing_capability().unwrap())
            .unwrap(),
    );
    let owner = ConfigAccess::local_as(runtime.clone(), runtime_identity.did().parse().unwrap());
    let reader = client.did().parse().unwrap();
    let did = gents::store::graphql::escape_graphql_string(runtime_identity.did());
    let mutation = |name: &str| {
        format!("mutation {{ create_Node(input: {{node_did: \"{did}\", display_name: \"{name}\", enabled: true, created_at: \"2026-11-04T00:00:00Z\", created_by: \"user\"}}) {{ _docID }} }}")
    };
    let created = access
        .write("test.observer.seed", &mutation("remote"))
        .await
        .unwrap();
    let physical = gents::store::graphql::created_doc_id(&created, "Node").unwrap();
    owner
        .add_actor_relationship("Node", &physical, "runtime_reader", &reader)
        .await
        .unwrap();
    let session = access
        .write(
            "test.observer.visible_session",
            &format!(
                r#"mutation {{ create_AgentSession(input: {{ node_did: "{did}", session_id: "unrelated-visible", requester_did: "{did}", agent_did: "{did}", created_at: "2026-11-04T00:00:00Z" }}) {{ _docID }} }}"#,
            ),
        )
        .await
        .unwrap();
    let session_doc = gents::store::graphql::created_doc_id(&session, "AgentSession").unwrap();
    owner
        .add_actor_relationship("AgentSession", &session_doc, "reader", &reader)
        .await
        .unwrap();
    ConfigAccess::local(desktop.clone())
        .write("test.observer.stale", &mutation("stale replica"))
        .await
        .unwrap();
    let mut peer = PeerRecord::new("runtime", "", runtime_identity.did());
    peer.graphql = Some(proxy_url);
    let mut peers = PeerDirectory::open_writer(directory.path().join("peers.json"))
        .await
        .unwrap();
    peers.upsert(peer).await.unwrap();
    let configured = ClientSyncStateOwner::new(Default::default(), peers, Vec::new());
    let (store, _) = ObservedStore::new(ClientStore::default());
    let handle = spawn_observer(
        store.clone(),
        configured,
        client.client_signing_capability().unwrap(),
    );
    wait_for_name(&store, Some("remote")).await;
    let mut notices = store.subscribe_changes();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store
                .snapshot()
                .sessions
                .iter()
                .any(|row| row.session_id == "unrelated-visible")
            {
                break;
            }
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("authorized session in authenticated snapshot");
    store.invalidate_node_after_denied_read(runtime_identity.did());
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store
                .snapshot()
                .sessions
                .iter()
                .any(|row| row.session_id == "unrelated-visible")
            {
                break;
            }
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("authorized session restored by authenticated resync");
    assert_eq!(
        store.runtime_connection(runtime_identity.did()),
        Some(RuntimeConnectionStatus::Connected)
    );
    let physical_id = physical;
    let physical = gents::store::graphql::escape_graphql_string(&physical_id);
    set_proxy_online(&proxy_commands, false).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Disconnected,
    )
    .await;
    access.write("test.observer.offline", &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"offline update\"}}) {{ _docID }} }}")).await.unwrap();
    assert_eq!(
        store.snapshot().nodes[0].display_name.as_deref(),
        Some("remote")
    );
    set_proxy_online(&proxy_commands, true).await;
    wait_for_name(&store, Some("offline update")).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Connected,
    )
    .await;
    let after_reconnect = handle.metrics_snapshot();
    access.write("test.observer.update", &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"updated\"}}) {{ _docID }} }}")).await.unwrap();
    wait_for_name(&store, Some("updated")).await;
    assert!(handle.metrics_snapshot().events_received > after_reconnect.events_received);
    set_proxy_online(&proxy_commands, false).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Disconnected,
    )
    .await;
    owner
        .delete_actor_relationship("Node", &physical_id, "runtime_reader", &reader)
        .await
        .unwrap();
    let hidden = reader_access
        .execute(&format!("{{ Node(docID: \"{physical}\") {{ _docID }} }}"))
        .await
        .unwrap();
    assert!(hidden["data"]["Node"].as_array().unwrap().is_empty());
    set_proxy_online(&proxy_commands, true).await;
    wait_for_name(&store, None).await;
    wait_for_connection(
        &store,
        runtime_identity.did(),
        RuntimeConnectionStatus::Connected,
    )
    .await;
    owner
        .add_actor_relationship("Node", &physical_id, "runtime_reader", &reader)
        .await
        .unwrap();
    wait_for_name(&store, Some("updated")).await;
    access
        .write(
            "test.observer.delete",
            &format!("mutation {{ delete_Node(docID: \"{physical}\") {{ _docID }} }}"),
        )
        .await
        .unwrap();
    wait_for_name(&store, None).await;
    assert_eq!(
        store.runtime_connection(runtime_identity.did()),
        Some(RuntimeConnectionStatus::Connected)
    );
    assert!(handle.metrics_snapshot().events_received >= 2);
    handle.shutdown().await;
    drop(proxy_commands);
    proxy_task.await.unwrap();
    desktop.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_endpoint_never_reads_runtime_data_from_desktop_store() {
    let desktop = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::client::schema::ensure_runtime_schemas(&desktop)
        .await
        .unwrap();
    ConfigAccess::local(desktop.clone()).write("test.observer.local", "mutation { create_Node(input: {node_did: \"did:test:runtime\", display_name: \"stale\", enabled: true, created_at: \"now\", created_by: \"old\"}) {_docID} }").await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let key_path = directory.path().join("client.key");
    let client =
        tokio::task::spawn_blocking(move || gents::KeyIdentity::load_or_create(key_path, None))
            .await
            .unwrap()
            .unwrap();
    let capability = client.client_signing_capability().unwrap();
    let snapshots =
        crate::client::query::load_full_snapshot_with_peer_records(&[], &capability).await;
    assert!(snapshots.store.nodes.is_empty());
    assert!(
        crate::client::query::load_node_scoped_snapshot_with_peer_records(
            "did:test:runtime",
            &[],
            &capability,
        )
        .await
        .is_err()
    );
    desktop.shutdown().await.unwrap();
}

#[test]
fn generated_runtime_connection_observations_drive_the_owner() {
    let output = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.RuntimeConnectionObservation:olean",
        "Proofs/Conformance/RuntimeConnectionObservation.lean",
    )
    .unwrap();
    let cases: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert!(!cases.is_empty());
    let (store, _) = ObservedStore::new(ClientStore::default());
    for case in cases {
        let status = match case["status"].as_str().unwrap() {
            "connecting" => RuntimeConnectionStatus::Connecting,
            "connected" => RuntimeConnectionStatus::Connected,
            "disconnected" => RuntimeConnectionStatus::Disconnected,
            "denied" => RuntimeConnectionStatus::Denied,
            other => panic!("unexpected status {other}"),
        };
        let event = match case["event"].as_str().unwrap() {
            "sourceStarted" => RuntimeConnectionEvent::SourceStarted,
            "subscriptionsReady" => RuntimeConnectionEvent::SubscriptionsReady,
            "snapshotSucceeded" => RuntimeConnectionEvent::SnapshotSucceeded,
            "transportFailed" => RuntimeConnectionEvent::TransportFailed,
            "permissionDenied" => RuntimeConnectionEvent::PermissionDenied,
            other => panic!("unexpected event {other}"),
        };
        store.set_runtime_connection("node", Some(status));
        store.observe_runtime_connection("node", event);
        assert_eq!(
            store.runtime_connection("node").unwrap().as_str(),
            case["expected"].as_str().unwrap()
        );
    }
}

#[tokio::test]
async fn denied_runtime_recovers_when_access_returns_at_the_same_endpoint() {
    let fixture = ObservedRuntime::start().await;
    let configured =
        ClientSyncStateOwner::new(Default::default(), fixture.peers.clone(), Vec::new());
    let (store, _) = ObservedStore::new(ClientStore::default());
    let handle = spawn_observer(
        store.clone(),
        configured,
        fixture.client.client_signing_capability().unwrap(),
    );
    wait_for_name(&store, Some("remote")).await;
    wait_for_connection(&store, &fixture.runtime_did, RuntimeConnectionStatus::Connected).await;

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Denied).await;
    wait_for_connection(&store, &fixture.runtime_did, RuntimeConnectionStatus::Denied).await;
    assert!(store.snapshot().nodes.is_empty());
    let physical = gents::store::graphql::escape_graphql_string(&fixture.physical);
    fixture
        .access
        .write(
            "test.observer.regranted",
            &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"regranted\"}}) {{ _docID }} }}"),
        )
        .await
        .unwrap();

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Online).await;
    wait_for_name(&store, Some("regranted")).await;
    wait_for_connection(&store, &fixture.runtime_did, RuntimeConnectionStatus::Connected).await;
    handle.shutdown().await;
    fixture.stop().await;
}

#[tokio::test]
async fn stalled_runtime_snapshot_delays_neither_another_runtime_nor_shutdown() {
    let fixture = ObservedRuntime::start().await;
    let (stalled_url, mut stalled_queries, stalled_task) = stalled_runtime().await;
    let mut peers = fixture.peers.clone();
    let mut stalled = PeerRecord::new("stalled", "", "did:test:stalled");
    stalled.graphql = Some(stalled_url);
    peers.upsert(stalled).await.unwrap();
    let configured = ClientSyncStateOwner::new(Default::default(), peers, Vec::new());
    let (store, _) = ObservedStore::new(ClientStore::default());
    let handle = spawn_observer(
        store.clone(),
        configured,
        fixture.client.client_signing_capability().unwrap(),
    );
    tokio::time::timeout(
        Duration::from_secs(15),
        stalled_queries.wait_for(|count| *count > 0),
    )
    .await
    .expect("stalled runtime snapshot in flight")
    .unwrap();
    wait_for_connection(&store, &fixture.runtime_did, RuntimeConnectionStatus::Connected).await;
    let mut notices = store.subscribe_changes();
    let physical = gents::store::graphql::escape_graphql_string(&fixture.physical);
    fixture
        .access
        .write(
            "test.observer.beside_stall",
            &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"beside stall\"}}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if store
                .snapshot()
                .nodes
                .iter()
                .any(|node| node.display_name.as_deref() == Some("beside stall"))
            {
                break;
            }
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("healthy runtime update beside a stalled snapshot");
    assert_eq!(
        store.runtime_connection(&fixture.runtime_did),
        Some(RuntimeConnectionStatus::Connected)
    );
    assert_eq!(
        store.runtime_connection("did:test:stalled"),
        Some(RuntimeConnectionStatus::Connecting)
    );
    tokio::time::timeout(Duration::from_secs(2), handle.shutdown())
        .await
        .expect("shutdown while a snapshot is stalled");
    stalled_task.abort();
    fixture.stop().await;
}

#[test]
fn generated_denied_reprobe_schedule_drives_the_sources() {
    let output = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.RuntimeConnectionReprobe:olean",
        "Proofs/Conformance/RuntimeConnectionReprobe.lean",
    )
    .unwrap();
    let cases: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let denials = u32::try_from(case["denials"].as_u64().unwrap()).unwrap();
        let event = match case["event"].as_str().unwrap() {
            "sourceStarted" => RuntimeConnectionEvent::SourceStarted,
            "subscriptionsReady" => RuntimeConnectionEvent::SubscriptionsReady,
            "snapshotSucceeded" => RuntimeConnectionEvent::SnapshotSucceeded,
            "transportFailed" => RuntimeConnectionEvent::TransportFailed,
            "permissionDenied" => RuntimeConnectionEvent::PermissionDenied,
            other => panic!("unexpected event {other}"),
        };
        assert_eq!(
            u64::from(remote::next_denials(denials, event)),
            case["nextDenials"].as_u64().unwrap(),
            "{case}"
        );
        assert_eq!(
            u64::try_from(remote::reprobe_delay(denials).as_millis()).unwrap(),
            case["delayMs"].as_u64().unwrap(),
            "{case}"
        );
    }
}
