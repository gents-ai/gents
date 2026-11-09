use std::collections::{BTreeMap, BTreeSet};

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
    Refusing,
    /// Keeps every armed subscription streaming and drops every other
    /// request: the runtime's reads fail for a transport reason.
    FailingReads,
}

type ProxyCommands =
    tokio::sync::mpsc::UnboundedSender<(ProxyMode, tokio::sync::oneshot::Sender<()>)>;

/// Reads the rest of a request whose head is `head`, so the refusal is sent
/// after the client finished writing.
async fn read_request_body(client: &mut tokio::net::TcpStream, head: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut request = head.to_vec();
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(request.len());
    let length = String::from_utf8_lossy(&request[..end])
        .to_ascii_lowercase()
        .lines()
        .find_map(|line| line.strip_prefix("content-length:").map(str::to_owned))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or_default();
    let mut buffer = [0_u8; 4096];
    while request.len() < end + 4 + length {
        let read = client.read(&mut buffer).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&buffer[..read]);
    }
    Ok(())
}

async fn request_head(client: &mut tokio::net::TcpStream) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = client.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    Some(head)
}

fn is_subscription(head: &[u8]) -> bool {
    String::from_utf8_lossy(head)
        .to_ascii_lowercase()
        .contains("accept: text/event-stream")
}

/// Asks the runtime to close a read's connection after its response, so the
/// client never reuses it for a subscription and every connection carries
/// requests of one kind.
fn single_request(head: Vec<u8>) -> Vec<u8> {
    let Some(line_end) = head.windows(2).position(|window| window == b"\r\n") else {
        return head;
    };
    let mut rewritten = head[..line_end + 2].to_vec();
    rewritten.extend_from_slice(b"connection: close\r\n");
    for line in head[line_end + 2..].split_inclusive(|byte| *byte == b'\n') {
        if !String::from_utf8_lossy(line)
            .to_ascii_lowercase()
            .starts_with("connection:")
        {
            rewritten.extend_from_slice(line);
        }
    }
    rewritten
}

/// Forwards a connection whose first bytes, `head`, were already read.
async fn forward(mut client: tokio::net::TcpStream, head: Vec<u8>, upstream: std::net::SocketAddr) {
    use tokio::io::AsyncWriteExt;
    if let Ok(mut server) = tokio::net::TcpStream::connect(upstream).await {
        if server.write_all(&head).await.is_ok() {
            let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
        }
    }
}

async fn interruptible_proxy(
    upstream: std::net::SocketAddr,
) -> (String, ProxyCommands, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let (commands, mut command_rx) =
        tokio::sync::mpsc::unbounded_channel::<(ProxyMode, tokio::sync::oneshot::Sender<()>)>();
    let task = tokio::spawn(async move {
        let mode = Arc::new(std::sync::Mutex::new(ProxyMode::Online));
        // A connection is classified by its first request: subscriptions and
        // reads are cut separately.
        let mut subscriptions = tokio::task::JoinSet::new();
        let mut reads = tokio::task::JoinSet::new();
        let (classified_tx, mut classified) =
            tokio::sync::mpsc::unbounded_channel::<(tokio::net::TcpStream, Vec<u8>)>();
        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    let Some((next, done)) = command else { break };
                    *mode.lock().unwrap() = next;
                    if next != ProxyMode::Online {
                        reads.abort_all();
                        while reads.join_next().await.is_some() {}
                    }
                    if !matches!(next, ProxyMode::Online | ProxyMode::FailingReads) {
                        subscriptions.abort_all();
                        while subscriptions.join_next().await.is_some() {}
                    }
                    let _ = done.send(());
                }
                accepted = listener.accept() => {
                    let (mut client, _) = accepted.unwrap();
                    let classified_tx = classified_tx.clone();
                    reads.spawn(async move {
                        if let Some(head) = request_head(&mut client).await {
                            let _ = classified_tx.send((client, head));
                        }
                    });
                }
                Some((mut client, head)) = classified.recv() => {
                    let current = *mode.lock().unwrap();
                    let subscription = is_subscription(&head);
                    match current {
                        ProxyMode::Online if subscription => {
                            subscriptions.spawn(forward(client, head, upstream));
                        }
                        ProxyMode::Online => {
                            reads.spawn(forward(client, single_request(head), upstream));
                        }
                        ProxyMode::FailingReads if subscription => {
                            subscriptions.spawn(forward(client, head, upstream));
                        }
                        ProxyMode::Refusing => {
                            reads.spawn(async move {
                                use tokio::io::AsyncWriteExt;
                                if read_request_body(&mut client, &head).await.is_ok() {
                                    let _ = client
                                        .write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                                        .await;
                                    let _ = client.shutdown().await;
                                }
                            });
                        }
                        ProxyMode::FailingReads | ProxyMode::Offline => {}
                    }
                }
                _ = subscriptions.join_next(), if !subscriptions.is_empty() => {}
                _ = reads.join_next(), if !reads.is_empty() => {}
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
    store.clear_node_after_refused_read(runtime_identity.did());
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
            "refusedFailure" => remote::StreamEnd::Failed { refused: true },
            "transportFailure" => remote::StreamEnd::Failed { refused: false },
            other => panic!("unexpected stream end {other}"),
        };
        let expected = case["event"].as_str().map(lean_event);
        assert_eq!(
            format!("{:?}", remote::stream_end_event(end)),
            format!("{expected:?}"),
            "{case}"
        );
    }
    let armed_events = contract["armedEvents"].as_array().unwrap();
    assert_eq!(armed_events.len(), 2);
    for case in armed_events {
        assert_eq!(
            remote::armed_event(case["renewal"].as_bool().unwrap()),
            lean_event(case["event"].as_str().unwrap()),
            "{case}"
        );
    }
    let armed = contract["armed"].as_array().unwrap();
    assert_eq!(armed.len(), 14);
    for case in armed {
        assert_eq!(
            remote::armed_after(
                case["wasArmed"].as_bool().unwrap(),
                lean_event(case["event"].as_str().unwrap())
            ),
            case["armed"].as_bool().unwrap(),
            "{case}"
        );
    }
    let outcomes = contract["snapshotOutcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 3);
    for case in outcomes {
        let outcome = match case["outcome"].as_str().unwrap() {
            "succeeded" => remote::SnapshotOutcome::Succeeded,
            "refused" => remote::SnapshotOutcome::Refused,
            "unreachable" => remote::SnapshotOutcome::Unreachable,
            other => panic!("unexpected snapshot outcome {other}"),
        };
        assert_eq!(
            remote::snapshot_event(outcome),
            lean_event(case["event"].as_str().unwrap()),
            "{case}"
        );
    }
    let base = Instant::now();
    let starts = contract["renewalStarts"].as_array().unwrap();
    assert!(!starts.is_empty());
    for case in starts {
        let started = base + Duration::from_millis(case["startedMs"].as_u64().unwrap());
        let next = remote::renewal_start(
            started,
            Duration::from_millis(case["armMs"].as_u64().unwrap()),
            gents::store::config_client::SUBSCRIPTION_RENEWAL_PERIOD,
        );
        assert_eq!(
            u64::try_from(next.duration_since(base).as_millis()).unwrap(),
            case["nextMs"].as_u64().unwrap(),
            "{case}"
        );
    }
    for (rust, lean) in [
        (gents::store::config_client::SUBSCRIPTION_RENEWAL_PERIOD, "renewalPeriodMs"),
        (SNAPSHOT_RETRY_DELAY, "snapshotRetryDelayMs"),
    ] {
        assert_eq!(
            u64::try_from(rust.as_millis()).unwrap(),
            contract[lean].as_u64().unwrap(),
            "{lean}"
        );
    }
}

/// The snapshot owner's failures map to `RuntimeConnectionObservation.SnapshotOutcome`:
/// only the access owner's classification is a refusal.
#[test]
fn snapshot_failures_classify_as_their_lean_outcome() {
    let refused = anyhow::Error::new(gents::store::config_client::ConfigClientError::Database(
        gents_protocol::database_error::GraphqlFailure::new(
            "snapshot",
            serde_json::json!([{"message":"opaque", "extensions":{"code":"PERMISSION_DENIED"}}]),
        ),
    ))
    .context("runtime desktop snapshot");
    let unreachable = anyhow::anyhow!("connection refused");
    assert_eq!(
        remote::snapshot_outcome(&refused.into()),
        remote::SnapshotOutcome::Refused
    );
    assert_eq!(
        remote::snapshot_outcome(&unreachable.into()),
        remote::SnapshotOutcome::Unreachable
    );
}

fn lean_status(name: &str) -> RuntimeConnectionStatus {
    match name {
        "connecting" => RuntimeConnectionStatus::Connecting,
        "connected" => RuntimeConnectionStatus::Connected,
        "disconnected" => RuntimeConnectionStatus::Disconnected,
        "refused" => RuntimeConnectionStatus::Refused,
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
        "refused" => RuntimeConnectionEvent::Refused,
        "renewed" => RuntimeConnectionEvent::Renewed,
        "endpointMissing" => RuntimeConnectionEvent::EndpointMissing,
        other => panic!("unexpected event {other}"),
    }
}

/// A runtime that refuses the client (HTTP 403 at every request) is named:
/// the rows are dropped, the status reads refused across every reconnect,
/// and the source reconnects on the backoff until the runtime accepts the
/// client again at the same endpoint.
#[tokio::test]
async fn refused_runtime_reconnects_until_access_returns_at_the_same_endpoint() {
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
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Refusing).await;
    wait_for_name(&store, None).await;
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Refused,
    )
    .await;
    assert!(store.snapshot().nodes.is_empty());
    let mut changes = store.subscribe_changes();
    tokio::time::timeout(Duration::from_secs(15), async {
        while handle.metrics_snapshot().reconnects < 3 {
            assert_eq!(
                store.runtime_connection(&fixture.runtime_did),
                Some(RuntimeConnectionStatus::Refused),
                "every reconnect of a refusing runtime keeps naming the refusal"
            );
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("a refusal reconnects on the backoff");
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
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
    handle.shutdown().await;
    fixture.stop().await;
}

/// Lean `reconnects .snapshot .transportFailed = false`: a snapshot read that
/// fails for a transport reason while every subscription stays armed shows
/// the outage and retries the read; it never ends the source, so the healthy
/// streams are not torn down and no reconnect is scheduled.
#[tokio::test]
async fn failed_snapshot_read_keeps_the_streams_armed() {
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
    let before = handle.metrics_snapshot();

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::FailingReads).await;
    let physical = gents::store::graphql::escape_graphql_string(&fixture.physical);
    fixture
        .access
        .write(
            "test.observer.read_outage",
            &format!("mutation {{ update_Node(docID: \"{physical}\", input: {{display_name: \"after read outage\"}}) {{ _docID }} }}"),
        )
        .await
        .unwrap();
    wait_for_connection(&store, &fixture.runtime_did, RuntimeConnectionStatus::Disconnected)
        .await;
    let during = handle.metrics_snapshot();
    assert!(during.fetch_failures > before.fetch_failures, "the read failed");
    assert_eq!(during.reconnects, before.reconnects, "a failed read never reconnects");

    set_proxy_mode(&fixture.proxy_commands, ProxyMode::Online).await;
    wait_for_name(&store, Some("after read outage")).await;
    wait_for_connection(&store, &fixture.runtime_did, RuntimeConnectionStatus::Connected).await;
    let after = handle.metrics_snapshot();
    assert_eq!(after.reconnects, before.reconnects, "the armed streams were kept");
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
    wait_for_connection(
        &store,
        &fixture.runtime_did,
        RuntimeConnectionStatus::Connected,
    )
    .await;
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
fn generated_reconnect_backoff_drives_the_sources() {
    let output = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.RuntimeConnectionBackoff:olean",
        "Proofs/Conformance/RuntimeConnectionBackoff.lean",
    )
    .unwrap();
    let contract: serde_json::Value = serde_json::from_str(&output).unwrap();
    let origin = |name: &str| match name {
        "source" => remote::Origin::Source,
        "snapshot" => remote::Origin::Snapshot,
        other => panic!("unexpected origin {other}"),
    };
    let reconnects = contract["reconnects"].as_array().unwrap();
    assert_eq!(reconnects.len(), 14);
    for case in reconnects {
        assert_eq!(
            remote::reconnects(
                origin(case["origin"].as_str().unwrap()),
                lean_event(case["event"].as_str().unwrap())
            ),
            case["reconnects"].as_bool().unwrap(),
            "{case}"
        );
    }
    let schedule = contract["schedule"].as_array().unwrap();
    assert!(schedule.len() > 100);
    for case in schedule {
        let attempts = u32::try_from(case["attempts"].as_u64().unwrap()).unwrap();
        let connected = case["connectedMs"].as_u64().map(Duration::from_millis);
        let attempt = remote::reconnect_attempt(attempts, connected);
        assert_eq!(u64::from(attempt), case["attempt"].as_u64().unwrap(), "{case}");
        assert_eq!(
            u64::try_from(remote::reconnect_delay(attempt).as_millis()).unwrap(),
            case["delayMs"].as_u64().unwrap(),
            "{case}"
        );
    }
    assert_eq!(
        u64::try_from(remote::RECONNECT_STABLE.as_millis()).unwrap(),
        contract["stableMs"].as_u64().unwrap()
    );
}

/// Every reconnect, after a transport failure or a refusal of the source or
/// a refused read, is scheduled by the one delay owner and counted until the
/// source stays connected for the stable period. A snapshot that failed for
/// a transport reason is not a reconnect: the source stays armed. A runtime
/// that connects and then drops at once keeps backing off.
#[tokio::test]
async fn every_reconnect_path_uses_the_backoff_and_only_a_stable_connection_resets_it() {
    let (store, _) = ObservedStore::new(ClientStore::default());
    let metrics = ObserverMetrics::default();
    let mut sources = remote::Sources::default();
    let node = "did:test:backoff";
    let base = Instant::now();
    let mut waits = Vec::new();
    let mut at = base;
    for (origin, event) in [
        (remote::Origin::Source, RuntimeConnectionEvent::TransportFailed),
        (remote::Origin::Source, RuntimeConnectionEvent::Refused),
        (remote::Origin::Snapshot, RuntimeConnectionEvent::Refused),
        (remote::Origin::Source, RuntimeConnectionEvent::TransportFailed),
        (remote::Origin::Source, RuntimeConnectionEvent::Refused),
        (remote::Origin::Snapshot, RuntimeConnectionEvent::Refused),
        (remote::Origin::Source, RuntimeConnectionEvent::TransportFailed),
    ] {
        sources.start_inert(node);
        // Each cycle connects and drops before the stable period.
        sources.observe(node, remote::Origin::Snapshot, RuntimeConnectionEvent::SnapshotSucceeded, &store, at, &metrics);
        at += Duration::from_secs(1);
        sources.observe(node, origin, event, &store, at, &metrics);
        let due = sources.next_reconnect().unwrap();
        waits.push(u64::try_from(due.duration_since(at).as_millis()).unwrap());
        assert!(!sources.take_due_reconnects(due - Duration::from_millis(1)));
        assert!(sources.take_due_reconnects(due));
        at = due;
    }
    assert_eq!(waits, vec![250, 250, 250, 250, 250, 500, 1000]);
    assert_eq!(metrics.snapshot().reconnects, 7);

    let generation = sources.start_inert(node);
    sources.observe(node, remote::Origin::Snapshot, RuntimeConnectionEvent::TransportFailed, &store, at, &metrics);
    let source = &sources.entries[node];
    assert_eq!(source.generation, generation);
    assert!(!source.ended(), "a failed read keeps the streams armed");
    assert_eq!(sources.next_reconnect(), None);
    assert_eq!(metrics.snapshot().reconnects, 7);

    sources.observe(node, remote::Origin::Snapshot, RuntimeConnectionEvent::SnapshotSucceeded, &store, at, &metrics);
    let stable = at + remote::RECONNECT_STABLE;
    sources.observe(node, remote::Origin::Source, RuntimeConnectionEvent::TransportFailed, &store, stable, &metrics);
    assert_eq!(
        sources.next_reconnect().unwrap().duration_since(stable),
        remote::reconnect_delay(0),
        "a connection that held for the stable period starts the count again"
    );
}

/// TLA `ReconnectsWaitTheBackoff`: a reconcile between a source task's end
/// and the loop applying its end notice keeps the source, so the notice
/// schedules the reconnect on the backoff instead of the reconcile restarting
/// the source at once.
#[tokio::test]
async fn reconcile_keeps_an_ended_source_until_its_notice_schedules_the_reconnect() {
    let (store, _) = ObservedStore::new(ClientStore::default());
    let store = Arc::new(store);
    let metrics = Arc::new(ObserverMetrics::default());
    let mut sources = remote::Sources::default();
    let (notices, mut notice_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut peer = PeerRecord::new("runtime", "", "did:test:ended");
    // Nothing listens on the discard port, so the first handshake fails.
    peer.graphql = Some("http://127.0.0.1:9/api/v0/graphql".into());
    let directory = tempfile::tempdir().unwrap();
    let client = tokio::task::spawn_blocking({
        let path = directory.path().join("client.key");
        move || KeyIdentity::load_or_create(path, None).unwrap()
    })
    .await
    .unwrap();
    let capability = client.client_signing_capability().unwrap();
    let peers = std::slice::from_ref(&peer);
    sources.reconcile(peers, &capability, &notices, &store, &metrics);
    let generation = sources.entries["did:test:ended"].generation;
    let notice = tokio::time::timeout(Duration::from_secs(15), notice_rx.recv())
        .await
        .expect("the failed handshake ends the task")
        .unwrap();
    assert_eq!(notice.event, RuntimeConnectionEvent::TransportFailed);
    assert_eq!(notice.generation, generation);

    sources.reconcile(peers, &capability, &notices, &store, &metrics);
    assert_eq!(
        sources.entries["did:test:ended"].generation, generation,
        "the ended source is kept for its notice"
    );
    let now = Instant::now();
    sources.observe(&notice.node, remote::Origin::Source, notice.event, &store, now, &metrics);
    assert_eq!(
        sources.next_reconnect().unwrap().duration_since(now),
        remote::reconnect_delay(0)
    );
    assert_eq!(
        store.runtime_connection("did:test:ended"),
        Some(RuntimeConnectionStatus::Disconnected)
    );
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

/// External premise for `RuntimeConnectionObservation.SnapshotOutcome`: the
/// pinned served runtime does not refuse a User whose household grant was
/// revoked. The SSE handshake is accepted and the open and new streams stay
/// silent, and a read answers without errors and without the selected roots,
/// so neither surface the access owner classifies (HTTP 401/403, a
/// `PERMISSION_DENIED` envelope) is produced. That is native ACP hiding what
/// the User may not read: the snapshot owner reads it as an empty snapshot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_user_is_answered_with_omitted_roots() {
    let home = crate::test_fixtures::ServedRuntimeHome::start().await;
    let access = home.user_graphql_access();
    let query = crate::client::query::snapshot_query(Some(home.node_did())).unwrap();
    let authorized = access.execute(&query).await.unwrap();
    assert!(authorized.get("errors").is_none(), "{authorized}");
    let mut answered: Vec<_> = authorized["data"].as_object().unwrap().keys().cloned().collect();
    answered.sort();
    let mut roots: Vec<_> = crate::client::query::SNAPSHOT_ROOTS
        .iter()
        .map(|root| (*root).to_owned())
        .collect();
    roots.sort();
    assert_eq!(answered, roots, "SNAPSHOT_ROOTS names every snapshot root");
    let before = crate::client::load_node_snapshot_on(&access, home.node_did())
        .await
        .unwrap();
    assert!(!before.nodes.is_empty());

    let subscription = "subscription { Node(showDeleted: true) { _docID _deleted } }";
    let mut open = access.subscribe(subscription).await.unwrap();
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
    let mut resubscribed = access.subscribe(subscription).await.unwrap();
    for stream in [&mut open, &mut resubscribed] {
        assert!(
            tokio::time::timeout(Duration::from_secs(2), stream.recv())
                .await
                .is_err(),
            "the stream stays silent after revocation"
        );
    }
    let hidden = crate::client::load_node_snapshot_on(&access, home.node_did())
        .await
        .expect("an omitted root is an empty read, not a failure");
    assert_eq!(hidden.row_count(), 0, "nothing is visible to the revoked User");
    home.shutdown().await;
}

/// Regression for the revoked-User retry storm: the served runtime shows a
/// revoked User an empty view at the resnapshot after its next renewal. The
/// observer reads it as connected with nothing visible, never shows an
/// outage or a denial, and never reconnects; a restored grant is read at the
/// next renewal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_user_sees_connected_with_nothing_and_never_reconnects() {
    let home = crate::test_fixtures::ServedRuntimeHome::start().await;
    let (_directory, store, handle) = served_observer(&home).await;
    wait_for_connection(&store, home.node_did(), RuntimeConnectionStatus::Connected).await;
    assert!(!store.snapshot().nodes.is_empty());
    let before = handle.metrics_snapshot();
    let user = ::identity::Did::new(home.user().did()).unwrap();
    gents::store::identity::GrantSet::HouseholdOwner
        .revoke(&home.runtime_access(), &user)
        .await
        .unwrap();
    let mut shown = vec![RuntimeConnectionStatus::Connected];
    let mut changes = store.subscribe_changes();
    let renewal = gents::store::config_client::SUBSCRIPTION_RENEWAL_PERIOD;
    tokio::time::timeout(renewal + Duration::from_secs(15), async {
        loop {
            if let Some(status) = store.runtime_connection(home.node_did()) {
                if shown.last() != Some(&status) {
                    shown.push(status);
                }
            }
            if store.snapshot().nodes.is_empty() {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the renewal resnapshot shows the empty view; shown {shown:?}"));
    let after = handle.metrics_snapshot();
    assert_eq!(shown, vec![RuntimeConnectionStatus::Connected], "{shown:?}");
    assert_eq!(
        store.runtime_connection(home.node_did()),
        Some(RuntimeConnectionStatus::Connected)
    );
    assert_eq!(after.reconnects, before.reconnects, "an empty view never reconnects");
    assert_eq!(after.transport_failures, before.transport_failures);

    gents::store::identity::GrantSet::HouseholdOwner
        .grant(&home.runtime_access(), &user)
        .await
        .unwrap();
    tokio::time::timeout(renewal + Duration::from_secs(15), async {
        while store.snapshot().nodes.is_empty() {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the next renewal resnapshot shows the restored grant");
    assert_eq!(
        store.runtime_connection(home.node_did()),
        Some(RuntimeConnectionStatus::Connected)
    );
    assert_eq!(handle.metrics_snapshot().reconnects, before.reconnects);
    handle.shutdown().await;
    home.shutdown().await;
}

/// Replays observer traces through the delivery path, the store fences, the
/// loop's notice and settlement handling (`apply_source_notice`,
/// `apply_settlement`, so the reconnect owner `Sources` runs as in the loop),
/// and checks every step against the executed Lean `ObserverResync` owner. A
/// slot is armed only while its source has not ended for a reconnect.
#[tokio::test]
async fn generated_observer_resync_traces_match_the_lean_owner() {
    use crate::client::store::ClientStoreRows;
    use gents_protocol::row::AgentRequestRow;
    use gents_protocol::schemas::{AGENT_MESSAGE_NAME, AGENT_TOOL_CALL_NAME};

    const NODES: usize = 2;
    let did = |node: usize| format!("did:resync:{node}");
    let session = |node: usize| {
        serde_json::from_value(serde_json::json!({
            "node_did": did(node), "session_id": format!("session-{node}"),
            "agent_did": "agent", "created_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    };
    let shared = |owner: Option<usize>| AgentRequestRow {
        doc_id: Some("shared".into()),
        request_id: "shared".into(),
        node_did: owner.map(did),
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        ..Default::default()
    };
    // `Conformance.ObserverResync.snapshot`.
    let snapshot = |node: usize, kind: &str| {
        let mut rows = ClientStoreRows {
            sessions: vec![session(node)],
            ..Default::default()
        };
        match kind {
            "foreign" => rows.sessions.push(session(1 - node)),
            "shared" => rows.requests.push(shared(Some(node))),
            _ => {}
        }
        ClientStore::from_rows(rows)
    };
    let op = |op: &str, node: usize, result: &str| serde_json::json!({"op": op, "node": node, "result": result});
    let alphabet = [
        op("armed", 0, ""),
        op("start", 0, ""),
        op("start", 1, ""),
        op("deliver", 0, ""),
        op("deliver", 1, ""),
        op("tool_call_event", 0, ""),
        op("transcript_event", 0, ""),
        op("stream_failed", 0, ""),
        op("stream_refused", 0, ""),
        op("source_started", 0, ""),
        op("publish", 0, "foreign"),
        op("publish", 0, "shared"),
        op("seed_unowned", 0, ""),
        op("timer", 0, ""),
        op("finish", 0, "rows"),
        op("finish", 0, "shared"),
        op("finish", 0, "transport_failed"),
        op("finish", 0, "refused"),
        op("finish", 1, "rows"),
    ];
    let prefix = [
        op("armed", 0, ""),
        op("armed", 1, ""),
        op("start", 0, ""),
        op("start", 1, ""),
    ];
    // An unowned row in the store before every load starts.
    let seeded = [
        op("armed", 0, ""),
        op("armed", 1, ""),
        op("seed_unowned", 0, ""),
        op("start", 0, ""),
        op("start", 1, ""),
    ];
    let suffix = [
        op("finish", 0, "rows"),
        op("timer", 0, ""),
        op("armed", 0, ""),
        op("start", 0, ""),
        op("finish", 0, "rows"),
        op("finish", 1, "rows"),
    ];
    let mut traces = Vec::new();
    for a in &alphabet {
        for b in &alphabet {
            let ops: Vec<_> = [a, b].into_iter().chain(suffix.iter()).cloned().collect();
            traces.push(serde_json::json!({"nodes": NODES, "ops": ops}));
            let ops: Vec<_> = seeded
                .iter()
                .chain([a, b])
                .chain(suffix.iter())
                .cloned()
                .collect();
            traces.push(serde_json::json!({"nodes": NODES, "ops": ops}));
            for c in &alphabet {
                let ops: Vec<_> = prefix
                    .iter()
                    .chain([a, b, c])
                    .chain(suffix.iter())
                    .cloned()
                    .collect();
                traces.push(serde_json::json!({"nodes": NODES, "ops": ops}));
            }
        }
    }
    let serialized = serde_json::to_string(&traces).unwrap();
    let output = tokio::task::spawn_blocking(move || {
        gents_lean_contract::run_prepared_generator_with_input(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.ObserverResync:olean",
            "Proofs/Conformance/ObserverResync.lean",
            &serialized,
        )
    })
    .await
    .unwrap()
    .unwrap();
    let modeled: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
    assert_eq!(modeled.len(), traces.len());

    let metrics = ObserverMetrics::default();
    let mut settlements = BTreeSet::new();
    for (trace, expected) in traces.iter().zip(&modeled) {
        let (store, _) = ObservedStore::new(ClientStore::default());
        let mut queue = Queue::default();
        let mut sources = remote::Sources::default();
        let mut generation = [0_u64; NODES];
        let mut in_flight = BTreeMap::<String, projection_store::StoreReadFence>::new();
        let now = Instant::now();
        for (node, generation) in generation.iter_mut().enumerate() {
            *generation = sources.start_inert(&did(node));
            store.observe_runtime_connection(&did(node), RuntimeConnectionEvent::SourceStarted);
        }
        let ops = trace["ops"].as_array().unwrap();
        let steps = expected["steps"].as_array().unwrap();
        assert_eq!(ops.len(), steps.len());
        for (step, modeled_step) in ops.iter().zip(steps) {
            let node = usize::try_from(step["node"].as_u64().unwrap()).unwrap();
            let node_did = did(node);
            let result = step["result"].as_str().unwrap();
            // What `observe/remote.rs` and the loop's notice arm do. A source
            // that ended for a reconnect sends nothing more: its next notice
            // comes from the source its due reconnect starts, which keeps the
            // shown status.
            let notice = |queue: &mut Queue,
                          sources: &mut remote::Sources,
                          in_flight: &mut BTreeMap<String, projection_store::StoreReadFence>,
                          generation: &mut u64,
                          event: RuntimeConnectionEvent| {
                if sources.entries[&node_did].ended() {
                    *generation = sources.start_inert(&node_did);
                }
                if SourceNotice::of(event) != Some(SourceNotice::Armed) {
                    store.observe_request_event(None);
                }
                let kind = apply_source_notice(
                    queue, sources, &store, &node_did, *generation, event, &metrics,
                )
                .expect("a source task event");
                if kind.aborts_load() {
                    in_flight.remove(&node_did);
                }
            };
            let mut finished = serde_json::Value::Null;
            match step["op"].as_str().unwrap() {
                "source_started" => {
                    generation[node] = sources.start_inert(&node_did);
                    in_flight.remove(&node_did);
                    store.observe_runtime_connection(
                        &node_did,
                        RuntimeConnectionEvent::SourceStarted,
                    );
                }
                "armed" => notice(
                    &mut queue,
                    &mut sources,
                    &mut in_flight,
                    &mut generation[node],
                    RuntimeConnectionEvent::SubscriptionsReady,
                ),
                "stream_failed" => notice(
                    &mut queue,
                    &mut sources,
                    &mut in_flight,
                    &mut generation[node],
                    RuntimeConnectionEvent::TransportFailed,
                ),
                "stream_refused" => notice(
                    &mut queue,
                    &mut sources,
                    &mut in_flight,
                    &mut generation[node],
                    RuntimeConnectionEvent::Refused,
                ),
                "start" => {
                    if queue.startable(&node_did, generation[node], in_flight.contains_key(&node_did))
                    {
                        queue.pending.remove(&node_did);
                        in_flight.insert(node_did.clone(), store.capture_read());
                    }
                }
                "deliver" | "tool_call_event" | "transcript_event" => {
                    let collection = match step["op"].as_str().unwrap() {
                        "deliver" => "AgentRequest",
                        "tool_call_event" => AGENT_TOOL_CALL_NAME,
                        _ => AGENT_MESSAGE_NAME,
                    };
                    delivery::deliver(
                        &store,
                        &metrics,
                        &node_did,
                        collection,
                        &serde_json::json!({"data": {collection: [{"_docID": "changed"}]}}),
                    )
                    .unwrap();
                    let (full, scopes) = store.take_resync_requests();
                    assert!(!full);
                    queue.pending.extend(scopes);
                }
                "publish" => {
                    assert!(store.replace_reloaded_snapshot_at_read(
                        &store.capture_read(),
                        Some(&node_did),
                        snapshot(node, result),
                    ));
                }
                "seed_unowned" => {
                    store.merge_chat_patch(ClientStore::from_rows(ClientStoreRows {
                        requests: vec![shared(None)],
                        ..Default::default()
                    }));
                }
                "timer" => {
                    queue.retry_at.remove(&node_did);
                }
                "finish" => {
                    if let Some(captured) = in_flight.remove(&node_did) {
                        let patch_accepted =
                            store.accepts_chat_patch_read(&captured, &snapshot(node, "rows"));
                        let settlement = match result {
                            "transport_failed" => Settlement::TransportFailed,
                            "refused" => {
                                clear_node(&store, &node_did);
                                Settlement::Refused
                            }
                            rows => {
                                if store.replace_reloaded_snapshot_at_read(
                                    &captured,
                                    Some(&node_did),
                                    snapshot(node, rows),
                                ) {
                                    Settlement::Published
                                } else {
                                    Settlement::Superseded
                                }
                            }
                        };
                        apply_settlement(
                            &mut queue,
                            &mut sources,
                            &store,
                            &node_did,
                            settlement,
                            now,
                            &metrics,
                        );
                        let name = match settlement {
                            Settlement::Published => "published",
                            Settlement::Superseded => "superseded",
                            Settlement::TransportFailed => "transport_failed",
                            Settlement::Refused => "refused",
                        };
                        settlements.insert(name);
                        finished = serde_json::json!({
                            "settlement": name, "patch_accepted": patch_accepted,
                        });
                    }
                }
                other => panic!("unknown modeled op {other}"),
            }
            assert_eq!(finished, modeled_step["result"], "{step} in {trace}");
            let slot = &modeled_step["slot"];
            let observed = serde_json::json!({
                "pending": queue.pending.contains(&node_did),
                "waiting": queue.retry_at.contains_key(&node_did),
                "loading": in_flight.contains_key(&node_did),
                "armed": queue.armed.get(&node_did) == Some(&generation[node])
                    && !sources.entries[&node_did].ended(),
                "status": store.runtime_connection(&node_did).unwrap().as_str(),
                "presentation": slot["presentation"],
            });
            assert_eq!(&observed, slot, "{step} in {trace}");
            assert_ne!(slot["presentation"], "stalled", "{step} in {trace}");
        }
        let mut rows: Vec<_> = expected["rows"].as_array().unwrap().clone();
        rows.sort_by_key(|row| row["key"].as_str().unwrap().to_owned());
        let owner = |node_did: Option<&str>| {
            node_did.map(|node_did| (0..NODES).find(|node| did(*node) == node_did).unwrap())
        };
        let snapshot = store.snapshot();
        let mut stored: Vec<_> = snapshot
            .sessions
            .iter()
            .map(|row| serde_json::json!({"key": row.session_id, "owner": owner(Some(&row.node_did))}))
            .chain(snapshot.requests.iter().map(|row| {
                serde_json::json!({"key": row.doc_id, "owner": owner(row.node_did.as_deref())})
            }))
            .collect();
        stored.sort_by_key(|row| row["key"].as_str().unwrap().to_owned());
        assert_eq!(stored, rows, "{trace}");
    }
    assert_eq!(settlements.len(), 4, "every settlement is exercised");
}

fn desktop_snapshot(body: &str) -> bool {
    body.contains("query DesktopSnapshot")
}

/// Every snapshot load sees runtime changes while it is in flight: one to its
/// own node's row and one adding another node's row on the same runtime. Both
/// arrive on the runtime's subscription as its node's events, and the reload
/// publishes only its node's rows. Each load still publishes, so the node
/// connects after its first load and the rows catch up to the last change.
#[tokio::test]
async fn request_events_during_every_snapshot_load_still_publish() {
    use crate::test_fixtures::{GatedRuntimeProxy, ServedRuntimeHome};

    let runtime = ServedRuntimeHome::start().await;
    runtime
        .publish_node(serde_json::json!({"display_name": "before"}))
        .await;
    let mut proxy = GatedRuntimeProxy::start(runtime.graphql(), desktop_snapshot).await;
    let directory = tempfile::tempdir().unwrap();
    let mut peers = PeerDirectory::open_writer(directory.path().join("peers.json"))
        .await
        .unwrap();
    let mut peer = PeerRecord::new("runtime", "", runtime.node_did());
    peer.graphql = Some(proxy.url().to_owned());
    peers.upsert(peer).await.unwrap();
    let configured = ClientSyncStateOwner::new(Default::default(), peers, Vec::new());
    let (store, _) = ObservedStore::new(ClientStore::default());
    let mut resyncs = store.subscribe_resync_requests();
    let handle = spawn_observer(
        store.clone(),
        configured,
        runtime.user().client_signing_capability().unwrap(),
    );
    let node = runtime.node_did().to_owned();
    for load in 1..=3_u64 {
        let held = proxy.next().await;
        assert!(
            held.body.contains(&node),
            "the held read is this node's snapshot"
        );
        runtime
            .publish_node(serde_json::json!({"display_name": format!("during load {load}")}))
            .await;
        resyncs.changed().await.unwrap();
        runtime.publish_foreign_node().await;
        resyncs.changed().await.unwrap();
        held.release();
        let mut notices = store.subscribe_changes();
        tokio::time::timeout(Duration::from_secs(15), async {
            while handle.metrics_snapshot().scope_reloads < load {
                notices.changed().await.unwrap();
            }
        })
        .await
        .expect("a load overlapping request events publishes");
        assert_eq!(
            store.runtime_connection(&node),
            Some(RuntimeConnectionStatus::Connected)
        );
    }
    let release = tokio::spawn(async move {
        loop {
            proxy.next().await.release();
        }
    });
    wait_for_name(&store, Some("during load 3")).await;
    handle.shutdown().await;
    release.abort();
    runtime.shutdown().await;
}
