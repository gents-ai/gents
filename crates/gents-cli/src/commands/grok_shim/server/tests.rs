use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex;
use tokio::time::{sleep, timeout, Instant};

use crate::commands::grok_shim::protocol::{read_server_envelope, write_client_envelope};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

mod connection;
mod socket;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);

type RecordedCapabilities = (bool, bool, bool, Option<String>);

struct RecordingDelegate {
    late_push: bool,
    payloads: Mutex<Vec<String>>,
    capabilities: Mutex<Option<RecordedCapabilities>>,
    client_ids: Mutex<Vec<u64>>,
    disconnects: AtomicUsize,
    instance_id: u64,
}

impl RecordingDelegate {
    fn new(late_push: bool, instance_id: u64) -> Arc<Self> {
        Arc::new(Self {
            late_push,
            payloads: Mutex::new(Vec::new()),
            capabilities: Mutex::new(None),
            client_ids: Mutex::new(Vec::new()),
            disconnects: AtomicUsize::new(0),
            instance_id,
        })
    }

    fn disconnect_count(&self) -> usize {
        self.disconnects.load(Ordering::SeqCst)
    }

    fn recorded_client_ids(&self) -> Vec<u64> {
        self.client_ids.lock().expect("client id log").clone()
    }
}

impl AcpDelegate for RecordingDelegate {
    fn handle_acp<'a>(
        &'a self,
        payload: &'a str,
        outbound: AcpOutbound,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.payloads
                .lock()
                .expect("payload log")
                .push(payload.to_string());
            outbound.send(payload.to_string()).await?;
            if self.late_push {
                let outbound = outbound.clone();
                tokio::spawn(async move {
                    sleep(Duration::from_millis(10)).await;
                    let _ = outbound
                        .send(
                            json!({
                                "jsonrpc": "2.0",
                                "method": "deferred",
                                "params": {},
                            })
                            .to_string(),
                        )
                        .await;
                });
            }
            Ok(())
        })
    }

    fn on_disconnect(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.disconnects.fetch_add(1, Ordering::SeqCst);
        })
    }
}

struct GatedDisconnectDelegate {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
    completed: AtomicBool,
}

impl GatedDisconnectDelegate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            completed: AtomicBool::new(false),
        })
    }
}

impl AcpDelegate for GatedDisconnectDelegate {
    fn handle_acp<'a>(
        &'a self,
        _payload: &'a str,
        _outbound: AcpOutbound,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn on_disconnect(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            self.completed.store(true, Ordering::SeqCst);
        })
    }
}

#[derive(Default)]
struct RecordingFactory {
    late_push: bool,
    delegates: Mutex<Vec<Arc<RecordingDelegate>>>,
    client_ids: Mutex<Vec<u64>>,
    capabilities: Mutex<Vec<ClientCapabilities>>,
}

impl RecordingFactory {
    fn constructed_count(&self) -> usize {
        self.delegates.lock().expect("delegate log").len()
    }

    fn constructed(&self) -> Vec<Arc<RecordingDelegate>> {
        self.delegates.lock().expect("delegate log").clone()
    }

    fn observed_client_ids(&self) -> Vec<u64> {
        self.client_ids.lock().expect("client id log").clone()
    }
}

impl AcpDelegateFactory for RecordingFactory {
    fn create_delegate<'a>(
        &'a self,
        client_id: u64,
        registration: &'a Registration,
    ) -> BoxFuture<'a, Result<Arc<dyn AcpDelegate>>> {
        Box::pin(async move {
            self.client_ids
                .lock()
                .expect("client id log")
                .push(client_id);
            self.capabilities
                .lock()
                .expect("capability log")
                .push(registration.capabilities.clone());
            let delegate =
                RecordingDelegate::new(self.late_push, (self.constructed_count() + 1) as u64);
            delegate
                .client_ids
                .lock()
                .expect("client id log")
                .push(client_id);
            *delegate.capabilities.lock().expect("capability log") = Some((
                registration.capabilities.yolo_mode,
                registration.capabilities.auto_mode,
                registration.capabilities.terminal,
                registration.capabilities.client_version.clone(),
            ));
            self.delegates
                .lock()
                .expect("delegate log")
                .push(delegate.clone());
            Ok(delegate as Arc<dyn AcpDelegate>)
        })
    }
}

#[derive(Default)]
struct FailingFactory {
    attempts: AtomicUsize,
}

impl AcpDelegateFactory for FailingFactory {
    fn create_delegate<'a>(
        &'a self,
        _client_id: u64,
        _registration: &'a Registration,
    ) -> BoxFuture<'a, Result<Arc<dyn AcpDelegate>>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("test factory refuses to construct a delegate"))
        })
    }
}

/// Canonicalize the chosen short test root: on macOS `/tmp` is a symlink
/// to `/private/tmp` and the socket-parent walk refuses symlinked
/// components, so resolve the platform's system temp symlink once here.
fn short_test_root() -> PathBuf {
    let explicit = Path::new("/tmp");
    if is_short_writable_root(explicit) {
        return std::fs::canonicalize(explicit).unwrap_or_else(|_| explicit.to_path_buf());
    }
    if let Ok(override_root) = std::env::var("GENTS_TEST_ROOT") {
        if !override_root.trim().is_empty() {
            let override_root = PathBuf::from(override_root);
            if is_short_writable_root(&override_root) {
                return std::fs::canonicalize(&override_root).unwrap_or(override_root);
            }
        }
    }
    let platform = std::env::temp_dir();
    if is_short_writable_root(&platform) {
        return std::fs::canonicalize(&platform).unwrap_or(platform);
    }
    // A short *relative* root anchored at cwd: sandboxed CI checkouts can
    // make every writable absolute location longer than `sun_path`, but
    // `sun_path` bounds the bind string, and relative and absolute paths
    // follow the identical leader code path.
    let workspace = PathBuf::from(".tmp-test-root");
    std::fs::create_dir_all(&workspace).expect("creating the workspace-local short test root");
    workspace
}

const MAX_SHORT_ROOT_BYTES: usize = 64;

fn is_short_writable_root(root: &Path) -> bool {
    let Ok(canonical) = std::fs::canonicalize(root) else {
        return false;
    };
    if canonical.as_os_str().len() > MAX_SHORT_ROOT_BYTES || !canonical.is_dir() {
        return false;
    }
    let probe = canonical.join(format!(
        ".gents-root-probe-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn unique_test_root(label: &str) -> PathBuf {
    let root = short_test_root().join(format!(
        "gents-grok-leader-{label}-{}",
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&root)
        .unwrap_or_else(|error| panic!("creating the test root {}: {error}", root.display()));
    root
}

fn long_filename_socket_path(root: &Path, target_bytes: usize) -> PathBuf {
    let file_name_len = target_bytes.saturating_sub(root.as_os_str().len() + 1);
    let stem_len = file_name_len.saturating_sub(".sock".len());
    if stem_len <= 8 {
        return root.join("s.sock");
    }
    root.join(format!("{}.sock", "f".repeat(stem_len)))
}

fn long_parent_socket_path(root: &Path, target_bytes: usize) -> PathBuf {
    let file_name = "s.sock";
    let component = "d".repeat(12);
    let mut parent = root.to_path_buf();
    loop {
        let candidate = parent.join(&component);
        if candidate.as_os_str().len() + 1 + file_name.len() > target_bytes {
            break;
        }
        parent = candidate;
    }
    let socket = parent.join(file_name);
    assert!(
        socket.as_os_str().len() + file_name.len() + 1 > target_bytes - 16,
        "the parent chain should really approach the limit"
    );
    socket
}

fn test_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        yolo_mode: true,
        auto_mode: false,
        default_model: Some("GLM-5.3-NVFP4".to_string()),
        client_version: Some("grok-pager-test".to_string()),
        code_nav_enabled: false,
        terminal: false,
        fs_read: false,
        fs_write: false,
        status_line: true,
    }
}

fn register_envelope() -> ClientEnvelope {
    ClientEnvelope::Register {
        client_type: "grok-pager".to_string(),
        mode: RegisterMode::Stdio,
        capabilities: test_capabilities(),
    }
}

async fn connect(socket: &Path) -> (OwnedReadHalf, OwnedWriteHalf) {
    let stream = timeout(TEST_TIMEOUT, UnixStream::connect(socket))
        .await
        .expect("connecting to the leader should not time out")
        .expect("connecting to the leader should succeed");
    stream.into_split()
}

async fn write_client_frame(writer: &mut OwnedWriteHalf, envelope: &ClientEnvelope) {
    timeout(TEST_TIMEOUT, write_client_envelope(writer, envelope))
        .await
        .expect("writing a client frame should not time out")
        .expect("writing a client frame should succeed");
}

async fn next_server_frame(reader: &mut OwnedReadHalf) -> Option<ServerEnvelope> {
    match timeout(TEST_TIMEOUT, read_server_envelope(reader))
        .await
        .expect("reading a server frame should not time out")
    {
        Ok(envelope) => Some(envelope),
        Err(error) if error.is_connection_closed() => None,
        Err(error) => panic!("reading a server frame should succeed: {error}"),
    }
}

async fn register(socket: &Path) -> (OwnedReadHalf, OwnedWriteHalf, u64) {
    let (mut reader, mut writer) = connect(socket).await;
    write_client_frame(&mut writer, &register_envelope()).await;
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Registered {
            client_id,
            ready,
            leader_protocol_version,
            leader_binary_version,
            ..
        }) => {
            assert!(ready, "registered must report ready");
            assert_eq!(leader_protocol_version, LEADER_PROTOCOL_VERSION);
            assert_eq!(
                leader_binary_version,
                format!("gents-{}", env!("CARGO_PKG_VERSION")),
                "registered must report the gents-prefixed package version"
            );
            assert!(client_id >= 1, "client ids start at 1");
            (reader, writer, client_id)
        }
        other => panic!("expected registered, got {other:?}"),
    }
}

fn assert_socket_mode_0600(socket: &Path) {
    let metadata = std::fs::metadata(socket).unwrap_or_else(|error| {
        panic!(
            "the published socket {} should exist: {error}",
            socket.display()
        )
    });
    assert!(
        metadata.file_type().is_socket(),
        "the published path should be a unix socket"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o777,
        0o600,
        "the published socket must be 0600"
    );
}

fn assert_clean_stop(handle: &LeaderHandle) {
    assert!(
        !handle.socket_path().exists(),
        "a clean stop must remove the published socket"
    );
    let lock = std::fs::symlink_metadata(handle.lock_path())
        .expect("a clean stop retains the stable leader lock inode");
    assert!(lock.file_type().is_file());
    assert_eq!(lock.permissions().mode() & 0o777, PRIVATE_FILE_MODE);
}

async fn exercise_leader(socket: &Path, factory: &RecordingFactory) {
    let (mut reader, mut writer, _client_id) = register(socket).await;

    write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Pong) => {}
        other => panic!("expected pong, got {other:?}"),
    }

    let payload = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "initialize",
        "params": {},
    })
    .to_string();
    write_client_frame(
        &mut writer,
        &ClientEnvelope::Acp {
            payload: payload.clone(),
        },
    )
    .await;
    match next_server_frame(&mut reader).await {
        Some(ServerEnvelope::Acp { payload: echoed }) => assert_eq!(echoed, payload),
        other => panic!("expected an acp echo, got {other:?}"),
    }
    let delegates = factory.constructed();
    let delegate = delegates
        .last()
        .expect("registration must have constructed a delegate");
    let payloads = delegate.payloads.lock().expect("payload log").clone();
    assert_eq!(
        payloads,
        vec![payload],
        "the connection delegate should have seen exactly the dispatched payload"
    );

    write_client_frame(&mut writer, &ClientEnvelope::Disconnect).await;
    assert!(
        next_server_frame(&mut reader).await.is_none(),
        "the leader should close the connection after disconnect"
    );
    assert!(
        delegate.disconnect_count() >= 1,
        "the connection delegate should observe the disconnect"
    );
}

async fn spawn_with_retry(socket: &Path, factory: Arc<RecordingFactory>) -> LeaderHandle {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        match spawn_leader(
            LeaderServerConfig::new(socket.to_path_buf()),
            factory.clone(),
        ) {
            Ok(handle) => return handle,
            Err(error) => {
                assert!(
                    Instant::now() < deadline,
                    "a leader should be spawnable after the previous handle was dropped: {error:#}"
                );
                sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

fn recording_factory() -> Arc<RecordingFactory> {
    Arc::new(RecordingFactory::default())
}
