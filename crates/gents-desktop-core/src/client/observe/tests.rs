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
    let contract: serde_json::Value = serde_json::from_str(&output).unwrap();
    let cases = contract["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    let (store, _) = ObservedStore::new(ClientStore::default());
    for case in cases {
        let status = lean_status(case["status"].as_str().unwrap());
        store.set_runtime_connection("node", Some(status));
        store.observe_runtime_connection("node", lean_event(case["event"].as_str().unwrap()));
        assert_eq!(
            store.runtime_connection("node").unwrap().as_str(),
            case["expected"].as_str().unwrap(),
            "{case}"
        );
    }
    let ends = contract["streamEnds"].as_array().unwrap();
    assert_eq!(ends.len(), 4);
    for case in ends {
        let end = match case["end"].as_str().unwrap() {
            "renewalDue" => remote::StreamEnd::Ended(gents::store::config_client::SubscriptionEnd::RenewalDue),
            "completed" => remote::StreamEnd::Ended(gents::store::config_client::SubscriptionEnd::Completed),
            "deniedFailure" => remote::StreamEnd::Failed { denied: true },
            "transportFailure" => remote::StreamEnd::Failed { denied: false },
            other => panic!("unexpected stream end {other}"),
        };
        let expected = case["event"].as_str().map(lean_event);
        assert_eq!(
            format!("{:?}", remote::stream_end_event(end)),
            format!("{expected:?}"),
            "{case}"
        );
    }
    assert_eq!(
        u64::try_from(RESYNC_RETRY_DELAY.as_millis()).unwrap(),
        contract["transportRetryDelayMs"].as_u64().unwrap()
    );
}

fn lean_status(name: &str) -> RuntimeConnectionStatus {
    match name {
        "connecting" => RuntimeConnectionStatus::Connecting,
        "connected" => RuntimeConnectionStatus::Connected,
        "disconnected" => RuntimeConnectionStatus::Disconnected,
        "denied" => RuntimeConnectionStatus::Denied,
        "noEndpoint" => RuntimeConnectionStatus::NoEndpoint,
        other => panic!("unexpected status {other}"),
    }
}

fn lean_event(name: &str) -> RuntimeConnectionEvent {
    match name {
        "sourceStarted" => RuntimeConnectionEvent::SourceStarted,
        "subscriptionsReady" => RuntimeConnectionEvent::SubscriptionsReady,
        "snapshotSucceeded" => RuntimeConnectionEvent::SnapshotSucceeded,
        "transportFailed" => RuntimeConnectionEvent::TransportFailed,
        "permissionDenied" => RuntimeConnectionEvent::PermissionDenied,
        "renewed" => RuntimeConnectionEvent::Renewed,
        "endpointMissing" => RuntimeConnectionEvent::EndpointMissing,
        other => panic!("unexpected event {other}"),
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
        let event = lean_event(case["event"].as_str().unwrap());
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

/// The User's observer of a served runtime, with the runtime's Node
/// published by the User.
async fn served_observer(
    home: &crate::test_fixtures::ServedRuntimeHome,
) -> (tempfile::TempDir, Arc<ObservedStore>, ObserverHandle) {
    let directory = tempfile::tempdir().unwrap();
    let mut peer = PeerRecord::new("runtime", "", home.node_did());
    peer.graphql = Some(home.graphql().to_owned());
    let mut peers = PeerDirectory::open_writer(directory.path().join("peers.json"))
        .await
        .unwrap();
    peers.upsert(peer).await.unwrap();
    let configured = ClientSyncStateOwner::new(Default::default(), peers, Vec::new());
    let (store, _) = ObservedStore::new(ClientStore::default());
    let handle = spawn_observer(
        store.clone(),
        configured,
        home.user().client_signing_capability().unwrap(),
    );
    (directory, store, handle)
}

async fn node_doc_id(home: &crate::test_fixtures::ServedRuntimeHome) -> String {
    let rows = home
        .user_graphql_access()
        .execute(&format!(
            "{{ Node(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
            gents::store::graphql::escape_graphql_string(home.node_did())
        ))
        .await
        .unwrap();
    rows["data"]["Node"][0]["_docID"].as_str().unwrap().to_owned()
}

/// Locally minted bearers last `DEFRADB_BEARER_LIFETIME` (60s) and each
/// subscription ends for renewal at half of it; this waits for a real one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn planned_renewal_keeps_the_runtime_connected() {
    let home = crate::test_fixtures::ServedRuntimeHome::start().await;
    let (_directory, store, handle) = served_observer(&home).await;
    wait_for_connection(&store, home.node_did(), RuntimeConnectionStatus::Connected).await;
    let before = handle.metrics_snapshot();
    let mut shown = vec![RuntimeConnectionStatus::Connected];
    let mut changes = store.subscribe_changes();
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            let metrics = handle.metrics_snapshot();
            if metrics.subscription_renewals > before.subscription_renewals
                || metrics.transport_failures > before.transport_failures
            {
                break;
            }
            tokio::select! {
                changed = changes.changed() => changed.unwrap(),
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
            if let Some(status) = store.runtime_connection(home.node_did()) {
                if shown.last() != Some(&status) {
                    shown.push(status);
                }
            }
        }
    })
    .await
    .expect("the subscriptions reach their renewal point");
    let after = handle.metrics_snapshot();
    assert_eq!(
        after.transport_failures, before.transport_failures,
        "a planned renewal is not a transport failure; shown {shown:?}"
    );
    assert!(after.subscription_renewals > before.subscription_renewals);
    let physical = node_doc_id(&home).await;
    home.user_graphql_access()
        .write(
            "test.observer.after_renewal",
            &format!(
                "mutation {{ update_Node(docID: \"{}\", input: {{display_name: \"after renewal\"}}) {{ _docID }} }}",
                gents::store::graphql::escape_graphql_string(&physical)
            ),
        )
        .await
        .unwrap();
    wait_for_name(&store, Some("after renewal")).await;
    assert_eq!(shown, vec![RuntimeConnectionStatus::Connected], "{shown:?}");
    assert_eq!(
        store.runtime_connection(home.node_did()),
        Some(RuntimeConnectionStatus::Connected)
    );
    handle.shutdown().await;
    home.shutdown().await;
}

#[tokio::test]
async fn peer_without_an_endpoint_shows_no_endpoint_until_it_has_one() {
    let directory = tempfile::tempdir().unwrap();
    let key_path = directory.path().join("client.key");
    let client =
        tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(key_path, None).unwrap())
            .await
            .unwrap();
    // An enrollment-only peer: discovery reached it, but it publishes no
    // operator GraphQL endpoint.
    let enrolled = PeerRecord::new("enrolled", "", "did:test:enrolled");
    let peers = PeerDirectory::open_writer(directory.path().join("peers.json"))
        .await
        .unwrap();
    let configured = ClientSyncStateOwner::new(Default::default(), peers, Vec::new());
    configured.upsert_for_test(enrolled.clone()).await.unwrap();
    let (store, _) = ObservedStore::new(ClientStore::default());
    let handle = spawn_observer(
        store.clone(),
        configured.clone(),
        client.client_signing_capability().unwrap(),
    );
    wait_for_connection(&store, "did:test:enrolled", RuntimeConnectionStatus::NoEndpoint).await;

    let (stalled_url, _queries, stalled_task) = stalled_runtime().await;
    let mut reachable = enrolled.clone();
    reachable.graphql = Some(stalled_url);
    configured.upsert_for_test(reachable).await.unwrap();
    wait_for_connection(&store, "did:test:enrolled", RuntimeConnectionStatus::Connecting).await;

    configured.upsert_for_test(enrolled.clone()).await.unwrap();
    wait_for_connection(&store, "did:test:enrolled", RuntimeConnectionStatus::NoEndpoint).await;

    let current = configured
        .records()
        .into_iter()
        .find(|record| record.node_did == "did:test:enrolled")
        .unwrap();
    configured.remove_for_test(&current).await.unwrap();
    let mut notices = store.subscribe_changes();
    tokio::time::timeout(Duration::from_secs(15), async {
        while store.runtime_connection("did:test:enrolled").is_some() {
            notices.changed().await.unwrap();
        }
    })
    .await
    .expect("a removed peer has no connection status");
    handle.shutdown().await;
    stalled_task.abort();
}

/// External premise for `RuntimeConnectionObservation.StreamEnd`: the pinned
/// served runtime does not refuse a User whose household grant was revoked.
/// The SSE handshake is accepted and the open and new streams stay silent,
/// and a read answers without errors and without the selected root, so
/// neither denial surface the access owner classifies (HTTP 401/403, a
/// `PERMISSION_DENIED` envelope) is produced.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_user_is_answered_without_a_classified_denial() {
    let home = crate::test_fixtures::ServedRuntimeHome::start().await;
    let access = home.user_graphql_access();
    let query = "subscription { Node(showDeleted: true) { _docID _deleted } }";
    let mut open = access.subscribe(query).await.unwrap();
    gents::store::identity::GrantSet::HouseholdOwner
        .revoke(
            &home.runtime_access(),
            &::identity::Did::new(home.user().did()).unwrap(),
        )
        .await
        .unwrap();
    let read = access.execute("{ Node { _docID } }").await.unwrap();
    assert!(read.get("errors").is_none(), "{read}");
    assert!(read["data"].get("Node").is_none(), "{read}");
    let mut resubscribed = access.subscribe(query).await.unwrap();
    for stream in [&mut open, &mut resubscribed] {
        assert!(
            tokio::time::timeout(Duration::from_secs(2), stream.recv())
                .await
                .is_err(),
            "the stream stays silent after revocation"
        );
    }
    home.shutdown().await;
}
