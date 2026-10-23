//! Grok shim leader server: the Unix-domain-socket leader that stock Grok
//! attaches to as its pager client.
//!
//! Gents binds the socket; the pager client connects to it. Election is
//! exclusive via a sibling extension-swapped lock (the same swap a stock Grok
//! leader performs) taken nonblocking-exclusive before anything is published;
//! a loser never removes or replaces the winner's lock or socket. The socket
//! is published atomically from a private `0700` staging ancestor, so a pager
//! client only ever observes a finished `0600` socket or no socket at all.
//! The first frame on a connection must be a valid `register`; delegates are
//! constructed per registered connection and `on_disconnect` runs only on the
//! delegate that connection constructed. On a clean stop the accept loop
//! announces `shutting_down` plus `shutdown` to every live connection,
//! releases the leader lock, and removes the socket, retaining the private
//! lock inode to avoid split-lock races. Dropping [`LeaderHandle`] without
//! shutting down is the emergency path: it aborts the listener task, unlinks
//! the published socket, and deliberately leaves the lock file, because
//! unlinking a lock file the dropper may no longer hold would let a new
//! leader lose exclusivity.
//!
//! Two deliberate adjustments to the leader contract, both forced by the
//! "no Cargo dependency change" rule for this shim: the nonblocking exclusive
//! lock is taken with `std::fs::File::try_lock` (Unix `flock(2)` `LOCK_EX |
//! LOCK_NB`), and `O_NOFOLLOW` is passed from a per-target constant declared
//! in this file with the same ABI value `libc`/`nix` use.

use std::fs::{File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

use super::protocol::{
    read_client_envelope, write_server_envelope, ClientCapabilities, ClientEnvelope,
    LeaderCapabilities, ProtocolError, RegisterMode, ServerEnvelope, ShutdownReason,
    LEADER_PROTOCOL_VERSION,
};

const LOG_TARGET: &str = "gents_cli::commands::grok_shim::server";

const ENVELOPE_ERROR_INVALID_REQUEST: i32 = -32600;

const ENVELOPE_ERROR_METHOD_NOT_FOUND: i32 = -32601;

const JSONRPC_INTERNAL_ERROR: i64 = -32603;

const PRIVATE_FILE_MODE: u32 = 0o600;

const STAGING_DIR_MODE: u32 = 0o700;

const SOCKET_PARENT_DIR_MODE: u32 = 0o700;

const STAGED_SOCKET_NAME: &str = "s";

const STAGING_DIR_PREFIX: &str = ".gents-grok-";

const STAGING_ATTEMPTS: usize = 8;

const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Internal bound for joining live connection tasks during leader shutdown,
/// independent of the pager-facing shutdown envelope whose delay remains zero.
const CONNECTION_CLEANUP_GRACE: Duration = Duration::from_secs(5);

const WIRE_SHUTDOWN_DELAY_MS: u64 = 0;

const OUTBOUND_FRAME_CAPACITY: usize = 256;

/// One decoded inbound envelope may wait while the connection loop services a
/// lifecycle event. The dedicated reader retains partial frame bytes across
/// `select!` cancellation in the connection loop.
const INBOUND_FRAME_CAPACITY: usize = 1;

/// Maximum concurrent ACP dispatches per connection: a prompt and its cancel
/// may overlap without an unbounded task fanout.
const MAX_ACP_DISPATCH_TASKS: usize = 64;

const LOCK_ACQUIRE_ATTEMPTS: usize = 8;

/// Published socket path length exercised by the near-limit tests; `sun_path`
/// is 104 bytes on macOS and 108 on Linux, so 100 bytes is near the limit
/// while remaining connectable on every supported target.
#[cfg(test)]
const NEAR_LIMIT_PATH_BYTES: usize = 100;

/// `O_NOFOLLOW` for `OpenOptions::custom_flags`. Unsupported Unix targets fail
/// the build rather than silently opening a lock path through a symlink.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
compile_error!("the grok shim leader lock needs this target's O_NOFOLLOW value; add it here");
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0x0000_8000;
#[cfg(any(target_os = "macos", target_os = "ios"))]
const O_NOFOLLOW: i32 = 0x0000_0040;

/// Connection-scoped outbound handle for ACP payloads.
#[derive(Clone)]
pub(crate) struct AcpOutbound {
    frames: FrameSender,
}

#[derive(Clone)]
enum FrameSender {
    Bounded(mpsc::Sender<ServerEnvelope>),
    #[cfg(test)]
    Unbounded(mpsc::UnboundedSender<ServerEnvelope>),
}

impl FrameSender {
    async fn send(&self, envelope: ServerEnvelope) -> Result<()> {
        match self {
            FrameSender::Bounded(frames) => frames
                .send(envelope)
                .await
                .map_err(|_| anyhow!("the grok shim leader connection is closed")),
            #[cfg(test)]
            FrameSender::Unbounded(frames) => frames
                .send(envelope)
                .map_err(|_| anyhow!("the grok shim leader connection is closed")),
        }
    }

    /// Nonblocking connection-loop send. Protocol/control/lifecycle frames
    /// cannot park the reader behind a saturated data queue; saturation is a
    /// fatal connection condition for these callers.
    fn try_send(&self, envelope: ServerEnvelope) -> Result<()> {
        match self {
            FrameSender::Bounded(frames) => {
                frames.try_send(envelope).map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => {
                        anyhow!("the grok shim leader outbound queue is full")
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        anyhow!("the grok shim leader connection is closed")
                    }
                })
            }
            #[cfg(test)]
            FrameSender::Unbounded(frames) => frames
                .send(envelope)
                .map_err(|_| anyhow!("the grok shim leader connection is closed")),
        }
    }
}

impl AcpOutbound {
    #[cfg(test)]
    pub(crate) fn for_frames(frames: mpsc::UnboundedSender<ServerEnvelope>) -> Self {
        Self {
            frames: FrameSender::Unbounded(frames),
        }
    }
    pub(crate) async fn send(&self, payload: impl Into<String>) -> Result<()> {
        self.frames
            .send(ServerEnvelope::Acp {
                payload: payload.into(),
            })
            .await
    }

    fn try_send(&self, payload: impl Into<String>) -> Result<()> {
        self.frames.try_send(ServerEnvelope::Acp {
            payload: payload.into(),
        })
    }
}

/// The ACP behavior behind the leader, implemented by `acp.rs`. Methods are
/// object-safe boxed futures because each ACP frame is dispatched on its own
/// task: a `session/prompt` handler must run to terminalization while the
/// connection keeps reading (a `session/cancel` frame must not wait behind
/// the prompt it cancels).
pub(crate) trait AcpDelegate: Send + Sync + 'static {
    fn handle_acp<'a>(
        &'a self,
        payload: &'a str,
        outbound: AcpOutbound,
    ) -> BoxFuture<'a, Result<()>>;

    /// The connection went away (disconnect, EOF, protocol violation, or
    /// leader shutdown). Runs on the connection's own delegate exactly once,
    /// before any other delegate is torn down.
    fn on_disconnect(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Registration {
    pub(crate) client_type: String,
    pub(crate) mode: RegisterMode,
    pub(crate) capabilities: ClientCapabilities,
}

pub(crate) trait AcpDelegateFactory: Send + Sync + 'static {
    fn create_delegate<'a>(
        &'a self,
        client_id: u64,
        registration: &'a Registration,
    ) -> BoxFuture<'a, Result<Arc<dyn AcpDelegate>>>;
}

impl<F> AcpDelegateFactory for F
where
    F: Fn(u64, &Registration) -> Result<Arc<dyn AcpDelegate>> + Send + Sync + 'static,
{
    fn create_delegate<'a>(
        &'a self,
        client_id: u64,
        registration: &'a Registration,
    ) -> BoxFuture<'a, Result<Arc<dyn AcpDelegate>>> {
        Box::pin(async move { self(client_id, registration) })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LeaderServerConfig {
    pub(crate) socket_path: PathBuf,
}

impl LeaderServerConfig {
    pub(crate) fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }
}

pub(crate) struct LeaderHandle {
    socket_path: PathBuf,
    shutdown_tx: watch::Sender<bool>,
    task: Option<JoinHandle<Result<()>>>,
}

impl LeaderHandle {
    #[cfg(test)]
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    #[cfg(test)]
    pub(crate) fn lock_path(&self) -> PathBuf {
        self.socket_path.with_extension("lock")
    }

    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.join().await
    }

    pub(crate) async fn join(&mut self) -> Result<()> {
        match self.task.take() {
            None => Ok(()),
            Some(task) => task
                .await
                .map_err(|error| anyhow!("the grok shim leader task failed to join: {error}"))
                .and_then(|result| result.context("the grok shim leader task failed")),
        }
    }
}

impl Drop for LeaderHandle {
    fn drop(&mut self) {
        let Some(task) = self.task.take() else {
            return;
        };
        task.abort();
        if let Err(error) = std::fs::remove_file(&self.socket_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    target: LOG_TARGET,
                    %error,
                    socket = %self.socket_path.display(),
                    "failed to unlink the grok shim socket while dropping a leader handle"
                );
            }
        }
    }
}

/// Must be called from within a Tokio runtime.
pub(crate) fn spawn_leader(
    config: LeaderServerConfig,
    factory: Arc<dyn AcpDelegateFactory>,
) -> Result<LeaderHandle> {
    let socket_path = normalize_platform_temp_root(&config.socket_path)?;
    if socket_path.file_name().is_none_or(|name| name.is_empty()) {
        bail!(
            "the grok shim leader socket path {} must name a socket file",
            socket_path.display()
        );
    }

    let lock = LeaderLock::acquire(&socket_path)?;

    if let Err(error) = remove_stale_socket(&socket_path) {
        lock.release();
        return Err(error);
    }

    let listener = match publish_listener(&socket_path) {
        Ok(listener) => listener,
        Err(error) => {
            lock.release();
            return Err(error);
        }
    };

    tracing::info!(
        target: LOG_TARGET,
        socket = %socket_path.display(),
        lock = %socket_path.with_extension("lock").display(),
        "grok shim leader listening for the pager client"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (lifecycle_tx, _lifecycle_rx) = broadcast::channel::<ServerEnvelope>(16);
    let task = tokio::spawn(accept_loop(
        listener,
        lock,
        socket_path.clone(),
        factory,
        lifecycle_tx,
        shutdown_rx,
    ));

    Ok(LeaderHandle {
        socket_path,
        shutdown_tx,
        task: Some(task),
    })
}

/// Resolve the two system temporary-directory aliases macOS installs at the
/// filesystem root (`/tmp -> /private/tmp` and `/var -> /private/var`). The
/// strict parent walk below must still reject every operator-controlled or
/// nested symlink; resolving only these OS-owned root aliases keeps that
/// security boundary while allowing `std::env::temp_dir()` on macOS.
fn normalize_platform_temp_root(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    for root in [Path::new("/tmp"), Path::new("/var")] {
        if !path.starts_with(root) {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspecting platform temporary root {}", root.display())
                });
            }
        };
        if !metadata.file_type().is_symlink() {
            return Ok(path.to_path_buf());
        }
        let canonical = std::fs::canonicalize(root).with_context(|| {
            format!("resolving platform temporary root alias {}", root.display())
        })?;
        let suffix = path.strip_prefix(root).with_context(|| {
            format!(
                "removing platform temporary root {} from {}",
                root.display(),
                path.display()
            )
        })?;
        return Ok(canonical.join(suffix));
    }
    Ok(path.to_path_buf())
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: UnixListener,
    lock: LeaderLock,
    socket_path: PathBuf,
    factory: Arc<dyn AcpDelegateFactory>,
    lifecycle: broadcast::Sender<ServerEnvelope>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let next_client_id = AtomicU64::new(1);
    let mut connections = JoinSet::new();

    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                // `Err` means the handle is gone, which is an implicit stop.
                let _ = changed;
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _address)) => {
                        let client_id = next_client_id.fetch_add(1, Ordering::Relaxed);
                        let lifecycle_rx = lifecycle.subscribe();
                        connections.spawn(handle_connection(
                            stream,
                            client_id,
                            factory.clone(),
                            lifecycle_rx,
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            %error,
                            "grok shim leader failed to accept a pager connection"
                        );
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    let _ = lifecycle.send(ServerEnvelope::ShuttingDown {
        reason: ShutdownReason::Manual,
        delay_ms: WIRE_SHUTDOWN_DELAY_MS,
    });
    let _ = lifecycle.send(ServerEnvelope::Shutdown);
    drop(lifecycle);

    let drain = async { while connections.join_next().await.is_some() {} };
    let cleanup_result = if tokio::time::timeout(CONNECTION_CLEANUP_GRACE, drain)
        .await
        .is_err()
    {
        tracing::warn!(
            target: LOG_TARGET,
            grace_ms = CONNECTION_CLEANUP_GRACE.as_millis(),
            "grok shim leader connection cleanup grace elapsed"
        );
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Err(anyhow!(
            "grok shim leader connection cleanup exceeded {}ms",
            CONNECTION_CLEANUP_GRACE.as_millis()
        ))
    } else {
        Ok(())
    };

    drop(listener);

    remove_published_socket(&socket_path);
    lock.release();
    tracing::info!(
        target: LOG_TARGET,
        socket = %socket_path.display(),
        "grok shim leader stopped cleanly"
    );
    cleanup_result
}

fn remove_published_socket(socket_path: &Path) {
    match std::fs::remove_file(socket_path) {
        Ok(()) => {
            tracing::debug!(
                target: LOG_TARGET,
                socket = %socket_path.display(),
                "removed the published grok shim socket on clean stop"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                socket = %socket_path.display(),
                "failed to remove the published grok shim socket on clean stop"
            );
        }
    }
}

async fn handle_connection(
    stream: UnixStream,
    client_id: u64,
    factory: Arc<dyn AcpDelegateFactory>,
    mut lifecycle: broadcast::Receiver<ServerEnvelope>,
) {
    let (mut reader, writer) = stream.into_split();
    let (frames_tx, mut frames_rx) = mpsc::channel::<ServerEnvelope>(OUTBOUND_FRAME_CAPACITY);
    let frames_tx = FrameSender::Bounded(frames_tx);
    let (client_frames_tx, mut client_frames_rx) =
        mpsc::channel::<std::result::Result<ClientEnvelope, ProtocolError>>(INBOUND_FRAME_CAPACITY);
    let reader_task = tokio::spawn(async move {
        loop {
            let frame = read_client_envelope(&mut reader).await;
            let terminal = frame.is_err();
            if client_frames_tx.send(frame).await.is_err() || terminal {
                break;
            }
        }
    });
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(envelope) = frames_rx.recv().await {
            if let Err(error) = write_server_envelope(&mut writer, &envelope).await {
                tracing::debug!(
                    target: LOG_TARGET,
                    %error,
                    "grok shim leader stopped writing to a pager connection"
                );
                break;
            }
        }
    });

    let registration =
        match register_client(&mut client_frames_rx, &frames_tx, &mut lifecycle).await {
            Ok(registration) => registration,
            Err(()) => {
                drop(frames_tx);
                reader_task.abort();
                let _ = reader_task.await;
                let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
                return;
            }
        };

    let delegate: Arc<dyn AcpDelegate> =
        match factory.create_delegate(client_id, &registration).await {
            Ok(delegate) => delegate,
            Err(error) => {
                tracing::error!(
                    target: LOG_TARGET,
                    client_id,
                    %error,
                    "grok shim leader could not construct an ACP delegate for a connection"
                );
                drop(frames_tx);
                reader_task.abort();
                let _ = reader_task.await;
                let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
                return;
            }
        };

    let _ = frames_tx.try_send(ServerEnvelope::Registered {
        client_id,
        ready: true,
        leader_protocol_version: LEADER_PROTOCOL_VERSION,
        leader_binary_version: leader_binary_version(),
        leader_capabilities: leader_capabilities(),
    });
    tracing::info!(
        target: LOG_TARGET,
        client_id,
        client_type = %registration.client_type,
        mode = registration.mode.wire_name(),
        yolo_mode = registration.capabilities.yolo_mode,
        auto_mode = registration.capabilities.auto_mode,
        terminal = registration.capabilities.terminal,
        "grok shim leader registered a pager client"
    );

    let outbound = AcpOutbound {
        frames: frames_tx.clone(),
    };
    let mut acp_tasks: JoinSet<()> = JoinSet::new();
    loop {
        reap_acp_dispatches(&mut acp_tasks);
        tokio::select! {
            biased;
            lifecycle_frame = lifecycle.recv() => {
                match lifecycle_frame {
                    Ok(envelope) => {
                        if frames_tx.try_send(envelope).is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            client_id,
                            missed,
                            "grok shim leader connection missed lifecycle frames; closing it"
                        );
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            frame = client_frames_rx.recv() => {
                match frame {
                    Some(Ok(ClientEnvelope::Ping)) => {
                        if frames_tx.try_send(ServerEnvelope::Pong).is_err() {
                            break;
                        }
                    }
                    Some(Ok(ClientEnvelope::Acp { payload })) => {
                        if !acp_dispatch_has_capacity(&acp_tasks) {
                            tracing::warn!(
                                target: LOG_TARGET,
                                client_id,
                                active = acp_tasks.len(),
                                "grok shim leader rejected ACP dispatch at the per-connection limit"
                            );
                            if let Some(response) = internal_error_response(&payload) {
                                if outbound.try_send(response).is_err() {
                                    break;
                                }
                            } else {
                                // Notifications have no response channel; closing the
                                // connection invokes `on_disconnect`, which cancels
                                // every pending turn.
                                break;
                            }
                        } else {
                            spawn_acp_dispatch(
                                &mut acp_tasks,
                                delegate.clone(),
                                outbound.clone(),
                                payload,
                            );
                        }
                    }
                    Some(Ok(ClientEnvelope::Control { request_id, command })) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            client_id,
                            request_id = %request_id,
                            command = %command,
                            "grok shim leader received an unsupported control command"
                        );
                        if frames_tx.try_send(ServerEnvelope::Error {
                            code: ENVELOPE_ERROR_METHOD_NOT_FOUND,
                            message: format!(
                                "the Gents leader shim does not implement control commands \
                                 (request {request_id:?}); leader_capabilities.control_v1 is false"
                            ),
                        }).is_err() {
                            break;
                        }
                    }
                    Some(Ok(ClientEnvelope::Register { .. })) => {
                        protocol_violation(
                            &frames_tx,
                            "register is only valid as the first frame on a connection",
                        );
                        break;
                    }
                    Some(Ok(ClientEnvelope::Disconnect)) => {
                        tracing::debug!(
                            target: LOG_TARGET,
                            client_id,
                            "grok shim leader pager client disconnected"
                        );
                        break;
                    }
                    Some(Err(error)) if error.is_connection_closed() => break,
                    Some(Err(error)) => {
                        tracing::debug!(
                            target: LOG_TARGET,
                            client_id,
                            %error,
                            "grok shim leader received an undecodable frame"
                        );
                        protocol_violation(
                            &frames_tx,
                            &format!("undecodable leader frame: {error}"),
                        );
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    // Connection cleanup: drain the delegate's state first so in-flight
    // handlers observe the drained turn table, then flush queued frames.
    delegate.on_disconnect().await;
    let drain = async { while acp_tasks.join_next().await.is_some() {} };
    if tokio::time::timeout(MAX_SHUTDOWN_GRACE, drain)
        .await
        .is_err()
    {
        tracing::warn!(
            target: LOG_TARGET,
            client_id,
            "grok shim leader closed a connection with ACP dispatch still running"
        );
        acp_tasks.abort_all();
        while acp_tasks.join_next().await.is_some() {}
    }
    drop(outbound);
    drop(frames_tx);
    reader_task.abort();
    let _ = reader_task.await;
    let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
}

/// Read frames until a valid `register` arrives. `Err(())` means the
/// connection must close; the error envelope (when the protocol requires one)
/// has already been queued.
async fn register_client(
    client_frames: &mut mpsc::Receiver<std::result::Result<ClientEnvelope, ProtocolError>>,
    frames: &FrameSender,
    lifecycle: &mut broadcast::Receiver<ServerEnvelope>,
) -> std::result::Result<Registration, ()> {
    let frame = tokio::select! {
        lifecycle_frame = lifecycle.recv() => {
            let _ = lifecycle_frame;
            // The leader is stopping before this client registered.
            return Err(());
        }
        frame = client_frames.recv() => match frame {
            Some(frame) => frame,
            None => return Err(()),
        },
    };
    match frame {
        Ok(ClientEnvelope::Register {
            client_type,
            mode,
            capabilities,
        }) => {
            if let Err(reason) = validate_register(&client_type, &mode) {
                tracing::warn!(
                    target: LOG_TARGET,
                    client_type = %client_type,
                    mode = %mode.wire_name(),
                    reason = %format!("{reason:#}"),
                    "grok shim leader rejected an invalid register frame"
                );
                let _ = frames.try_send(ServerEnvelope::Error {
                    code: ENVELOPE_ERROR_INVALID_REQUEST,
                    message: format!("{reason:#}"),
                });
                return Err(());
            }
            Ok(Registration {
                client_type,
                mode,
                capabilities,
            })
        }
        Ok(_) => {
            protocol_violation(
                frames,
                "the first frame on a leader connection must be register",
            );
            Err(())
        }
        Err(error) if error.is_connection_closed() => {
            tracing::debug!(
                target: LOG_TARGET,
                "grok shim leader connection closed before register"
            );
            Err(())
        }
        Err(error) => {
            protocol_violation(
                frames,
                &format!("undecodable leader frame before register: {error}"),
            );
            Err(())
        }
    }
}

fn validate_register(client_type: &str, _mode: &RegisterMode) -> Result<()> {
    if client_type.trim().is_empty() {
        bail!("register requires a non-empty client_type");
    }
    Ok(())
}

fn protocol_violation(frames: &FrameSender, message: &str) {
    tracing::warn!(target: LOG_TARGET, message, "grok shim leader protocol violation");
    let _ = frames.try_send(ServerEnvelope::Error {
        code: ENVELOPE_ERROR_INVALID_REQUEST,
        message: message.to_string(),
    });
}

fn reap_acp_dispatches(tasks: &mut JoinSet<()>) {
    while let Some(result) = tasks.try_join_next() {
        if let Err(error) = result {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                "grok shim leader ACP dispatch task failed to join"
            );
        }
    }
}

fn acp_dispatch_has_capacity(tasks: &JoinSet<()>) -> bool {
    tasks.len() < MAX_ACP_DISPATCH_TASKS
}

fn spawn_acp_dispatch(
    tasks: &mut JoinSet<()>,
    delegate: Arc<dyn AcpDelegate>,
    outbound: AcpOutbound,
    payload: String,
) {
    tasks.spawn(async move {
        if let Err(error) = delegate.handle_acp(&payload, outbound.clone()).await {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                "grok shim leader ACP dispatch failed"
            );
            // A request must never hang: answer it with a JSON-RPC internal
            // error through the ACP channel so the pager can recover.
            if let Some(response) = internal_error_response(&payload) {
                if let Err(error) = outbound.send(response).await {
                    tracing::debug!(
                        target: LOG_TARGET,
                        %error,
                        "grok shim leader could not deliver an ACP failure response"
                    );
                }
            }
        }
    });
}

fn internal_error_response(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let id = value.get("id")?;
    // Only requests are answered; notifications expect no response.
    value.get("method")?;
    Some(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": JSONRPC_INTERNAL_ERROR,
                "message": "Gents leader shim ACP dispatch failed",
            },
        })
        .to_string(),
    )
}

fn leader_binary_version() -> String {
    format!("gents-{}", env!("CARGO_PKG_VERSION"))
}

fn leader_capabilities() -> LeaderCapabilities {
    LeaderCapabilities {
        control_v1: false,
        runtime_cpu_profile: false,
        profile_formats: Vec::new(),
        workspace_exposure: false,
        relaunch_v1: false,
    }
}

/// Ensure every component of `parent` exists as a plain directory.
///
/// Deliberately not `create_dir_all`, which walks the path through symlinks:
/// existing directories are accepted with their own mode (never chmodmed),
/// symlinked and non-directory components are rejected, and missing components
/// are created `0700` from creation and verified with `symlink_metadata`.
fn ensure_socket_parent(parent: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.mode(SOCKET_PARENT_DIR_MODE);
    let mut current = PathBuf::new();
    for component in parent.components() {
        use std::path::Component;
        match component {
            Component::Prefix(_) | Component::RootDir => {
                current.push(component.as_os_str());
                continue;
            }
            _ => current.push(component.as_os_str()),
        }
        // Lstat: never follows a symlink component.
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                if file_type.is_symlink() {
                    bail!(
                        "refusing the grok shim socket parent {}: {} is a symlink",
                        parent.display(),
                        current.display()
                    );
                }
                if !file_type.is_dir() {
                    bail!(
                        "refusing the grok shim socket parent {}: {} exists and is not a directory",
                        parent.display(),
                        current.display()
                    );
                }
                continue;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "inspecting the grok shim socket parent component {}",
                        current.display()
                    )
                });
            }
        }
        builder.create(&current).with_context(|| {
            format!("creating the grok shim socket parent {}", current.display())
        })?;
        let created = std::fs::symlink_metadata(&current).with_context(|| {
            format!(
                "verifying the newly created grok shim socket parent {}",
                current.display()
            )
        })?;
        if created.file_type().is_symlink() || !created.file_type().is_dir() {
            bail!(
                "the grok shim socket parent component {} was created but is not a plain directory",
                current.display()
            );
        }
    }
    Ok(())
}

/// The exclusive sibling leader lock at `socket_path.with_extension("lock")`:
/// opened `O_NOFOLLOW`, forced `0600`, locked nonblocking-exclusive with
/// `File::try_lock` (`flock(2)` `LOCK_EX | LOCK_NB` on Unix). The open `File`
/// is the guard; the holder PID is written for diagnostics.
struct LeaderLock {
    path: PathBuf,
    file: File,
}

impl LeaderLock {
    fn acquire(socket_path: &Path) -> Result<Self> {
        let path = socket_path.with_extension("lock");
        if let Some(parent) = path.parent() {
            ensure_socket_parent(parent).with_context(|| {
                format!(
                    "creating the grok shim leader lock parent {}",
                    parent.display()
                )
            })?;
        }
        for _ in 0..LOCK_ACQUIRE_ATTEMPTS {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(PRIVATE_FILE_MODE)
                .custom_flags(O_NOFOLLOW)
                .open(&path)
                .with_context(|| format!("opening the grok shim leader lock {}", path.display()))?;
            if let Err(error) = file.try_lock() {
                if matches!(error, std::fs::TryLockError::WouldBlock) {
                    let holder = read_holder_pid(&path);
                    return Err(anyhow!(
                        "another grok leader already holds {} (last recorded holder pid: {})",
                        path.display(),
                        holder
                            .map(|pid| pid.to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                    ));
                }
                return Err(anyhow!(
                    "locking the grok shim leader lock {} failed: {error}",
                    path.display()
                ));
            }
            if file_identity(&file) != path_identity(&path) {
                drop(file);
                continue;
            }
            force_private_mode(&file, &path)?;
            let mut file = file;
            write_holder_pid(&mut file)
                .with_context(|| format!("recording the leader pid in {}", path.display()))?;
            // Revalidate after every mutation of the descriptor. A replaced
            // pathname never becomes the published election point.
            if file_identity(&file) != path_identity(&path) {
                drop(file);
                continue;
            }
            return Ok(Self { path, file });
        }
        bail!(
            "the grok shim leader lock {} changed inode during acquisition",
            path.display()
        )
    }

    /// Release by dropping the descriptor while retaining the stable private
    /// inode: removing a flock file before (or while) dropping the descriptor
    /// lets a concurrent opener race past the exclusive lock.
    fn release(self) {
        tracing::debug!(target: LOG_TARGET, lock = %self.path.display(), "released the grok shim leader lock");
        drop(self.file);
    }
}

/// Force `0600` on the open lock file and verify it through the descriptor:
/// `open(2)`'s mode is masked by the umask (and ignored for existing files),
/// and descriptor-based verification fails closed if the path was swapped.
fn force_private_mode(file: &File, path: &Path) -> Result<()> {
    let current = file
        .metadata()
        .with_context(|| {
            format!(
                "reading the mode of the grok shim leader lock {}",
                path.display()
            )
        })?
        .permissions()
        .mode();
    if current & 0o777 == PRIVATE_FILE_MODE {
        return Ok(());
    }
    file.set_permissions(Permissions::from_mode(PRIVATE_FILE_MODE))
        .with_context(|| {
            format!(
                "forcing mode 0600 on the grok shim leader lock {}",
                path.display()
            )
        })?;
    let forced = file
        .metadata()
        .with_context(|| {
            format!(
                "re-reading the mode of the grok shim leader lock {}",
                path.display()
            )
        })?
        .permissions()
        .mode();
    if forced & 0o777 != PRIVATE_FILE_MODE {
        bail!(
            "the grok shim leader lock {} is mode {:o} instead of 0600",
            path.display(),
            forced & 0o777
        );
    }
    Ok(())
}

fn file_identity(file: &File) -> Option<(u64, u64)> {
    let metadata = file.metadata().ok()?;
    Some((metadata.dev(), metadata.ino()))
}

fn path_identity(path: &Path) -> Option<(u64, u64)> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return None;
    }
    Some((metadata.dev(), metadata.ino()))
}

fn read_holder_pid(lock_path: &Path) -> Option<u32> {
    std::fs::read_to_string(lock_path).ok()?.trim().parse().ok()
}

fn write_holder_pid(file: &mut File) -> Result<()> {
    file.set_len(0)
        .with_context(|| "truncating the grok shim leader lock".to_string())?;
    file.write_all(format!("{}\n", process::id()).as_bytes())
        .with_context(|| "writing the leader pid into the lock file".to_string())?;
    Ok(())
}

/// Remove a stale socket left behind by a crashed leader; only called under
/// the exclusive lock, so a live leader's socket is never touched. Anything
/// that is not a plain socket file is refused.
fn remove_stale_socket(socket_path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(socket_path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                bail!(
                    "refusing to replace the symlink at {} with a grok shim leader socket",
                    socket_path.display()
                );
            }
            if !file_type.is_socket() {
                bail!(
                    "{} exists and is not a unix socket; refusing to replace it",
                    socket_path.display()
                );
            }
            std::fs::remove_file(socket_path).with_context(|| {
                format!(
                    "removing the stale grok shim socket {}",
                    socket_path.display()
                )
            })?;
            tracing::debug!(
                target: LOG_TARGET,
                socket = %socket_path.display(),
                "removed a stale grok shim leader socket under the exclusive leader lock"
            );
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspecting the grok shim socket path {}",
                    socket_path.display()
                )
            });
        }
    }
    Ok(())
}

/// Publish the leader socket atomically: bind inside a private `0700` short
/// same-device staging ancestor (so binding never depends on the published
/// path length), force the socket `0600` while it is still unreachable, then
/// `rename(2)` onto the published path.
fn publish_listener(socket_path: &Path) -> Result<UnixListener> {
    let parent = socket_path.parent().ok_or_else(|| {
        anyhow!(
            "the grok shim leader socket path {} has no parent directory",
            socket_path.display()
        )
    })?;
    ensure_socket_parent(parent).with_context(|| {
        format!(
            "creating the grok shim leader socket parent {}",
            parent.display()
        )
    })?;
    let staging = StagingDir::create(parent)?;
    let result = bind_and_publish(&staging.path, socket_path);
    staging.cleanup();
    result
}

fn bind_and_publish(staging_dir: &Path, socket_path: &Path) -> Result<UnixListener> {
    let staged_socket = staging_dir.join(STAGED_SOCKET_NAME);
    let listener = UnixListener::bind(&staged_socket).with_context(|| {
        format!(
            "binding the grok shim socket inside the staging ancestor {}",
            staging_dir.display()
        )
    })?;
    if let Err(error) = force_socket_mode(&staged_socket) {
        let _ = std::fs::remove_file(&staged_socket);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staged_socket, socket_path) {
        let _ = std::fs::remove_file(&staged_socket);
        return Err(error).with_context(|| {
            format!(
                "publishing the grok shim socket at {}",
                socket_path.display()
            )
        });
    }
    Ok(listener)
}

fn force_socket_mode(socket: &Path) -> Result<()> {
    std::fs::set_permissions(socket, Permissions::from_mode(PRIVATE_FILE_MODE)).with_context(
        || {
            format!(
                "forcing mode 0600 on the grok shim socket {}",
                socket.display()
            )
        },
    )?;
    let mode = std::fs::metadata(socket)
        .with_context(|| {
            format!(
                "reading the mode of the grok shim socket {}",
                socket.display()
            )
        })?
        .permissions()
        .mode();
    if mode & 0o777 != PRIVATE_FILE_MODE {
        bail!(
            "the grok shim socket {} is mode {:o} instead of 0600",
            socket.display(),
            mode & 0o777
        );
    }
    Ok(())
}

/// The private short same-device staging ancestor for one publication.
struct StagingDir {
    path: PathBuf,
}

impl StagingDir {
    /// Same-device ancestors, shortest first, so `rename(2)` stays on one
    /// filesystem and the bind path stays short near the `sun_path` limit.
    fn create(final_parent: &Path) -> Result<Self> {
        let mut last_error: Option<std::io::Error> = None;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(STAGING_DIR_MODE);
        for ancestor in staging_candidate_ancestors(final_parent) {
            for _ in 0..STAGING_ATTEMPTS {
                let candidate = ancestor.join(staging_dir_name());
                match builder.create(&candidate) {
                    Ok(()) => {
                        let mode = std::fs::metadata(&candidate)
                            .with_context(|| {
                                format!(
                                    "inspecting the grok shim staging ancestor {}",
                                    candidate.display()
                                )
                            })?
                            .permissions()
                            .mode();
                        if mode & 0o777 != STAGING_DIR_MODE {
                            let _ = std::fs::remove_dir(&candidate);
                            bail!(
                                "the grok shim staging ancestor {} is mode {:o} instead of 0700",
                                candidate.display(),
                                mode & 0o777
                            );
                        }
                        tracing::debug!(
                            target: LOG_TARGET,
                            staging = %candidate.display(),
                            "created a private staging ancestor for the grok shim socket"
                        );
                        return Ok(Self { path: candidate });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => {
                        last_error = Some(error);
                        break;
                    }
                }
            }
        }
        bail!(
            "no writable same-device staging ancestor for {}; last error: {}",
            final_parent.display(),
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "none".to_string())
        )
    }

    fn cleanup(self) {
        if let Err(error) = std::fs::remove_dir(&self.path) {
            tracing::warn!(
                target: LOG_TARGET,
                %error,
                staging = %self.path.display(),
                "failed to remove the grok shim staging ancestor"
            );
        }
    }
}

/// Same-device non-root ancestors of `final_parent`, shortest first. `/` is
/// never a candidate (a privileged process must not publish shim directories
/// at the filesystem root); cross-device ancestors cannot `rename(2)` onto the
/// final path.
fn staging_candidate_ancestors(final_parent: &Path) -> Vec<PathBuf> {
    let parent_dev = match std::fs::metadata(final_parent) {
        Ok(metadata) => metadata.dev(),
        Err(_) => return Vec::new(),
    };
    let mut candidates = Vec::new();
    for ancestor in final_parent.ancestors() {
        if ancestor == Path::new("/") {
            continue;
        }
        if let Ok(metadata) = std::fs::metadata(ancestor) {
            if metadata.dev() == parent_dev {
                candidates.push(ancestor.to_path_buf());
            }
        }
    }
    candidates.sort_by_key(|path| path.as_os_str().len());
    candidates
}

fn staging_dir_name() -> String {
    format!(
        "{STAGING_DIR_PREFIX}{}",
        &Uuid::new_v4().simple().to_string()[..8]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::sync::Mutex;
    use tokio::time::{sleep, timeout, Instant};

    use crate::commands::grok_shim::protocol::{read_server_envelope, write_client_envelope};
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

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

    #[cfg(unix)]
    #[test]
    fn the_staging_dir_is_0700_from_creation_and_rejects_unsafe_parents() {
        let root = unique_test_root("staging-create-mode");
        let staging = StagingDir::create(&root).expect("creating the staging ancestor");
        assert_eq!(
            std::fs::metadata(&staging.path)
                .expect("staging metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the staging ancestor must be 0700 from creation"
        );
        staging.cleanup();
        let _ = std::fs::remove_dir_all(&root);

        let root = unique_test_root("staging-symlink-parent");
        let target = root.join("real");
        let link = root.join("link");
        std::fs::create_dir(&target).expect("creating the symlink target");
        std::os::unix::fs::symlink(&target, &link).expect("creating the parent symlink");
        let socket = link.join("leader.sock");
        assert!(
            publish_listener(&socket).is_err(),
            "publication through a symlinked parent must be refused"
        );
        assert!(
            !target.join("leader.sock").exists(),
            "nothing may be published through the symlink"
        );
        let leaked: Vec<_> = std::fs::read_dir(&target)
            .expect("target readable")
            .filter_map(std::result::Result::ok)
            .collect();
        assert!(
            leaked.is_empty(),
            "no staging directory may be created through the symlinked parent"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn nested_missing_socket_parents_are_created_0700() {
        let root = unique_test_root("parent-nested");
        let parent = root.join("a").join("b").join("c");
        ensure_socket_parent(&parent).expect("the nested parent chain must be created");
        for component in [root.join("a"), root.join("a").join("b"), parent.clone()] {
            let mode = std::fs::metadata(&component)
                .unwrap_or_else(|error| {
                    panic!("component {} must exist: {error}", component.display())
                })
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode,
                0o700,
                "component {} must be created 0700 from creation",
                component.display()
            );
        }
        ensure_socket_parent(&parent).expect("existing components must be accepted");
        let mode = std::fs::metadata(&parent)
            .expect("parent metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pre_existing_socket_parent_keeps_its_own_mode() {
        let root = unique_test_root("parent-mode");
        let parent = root.join("owned");
        std::fs::create_dir(&parent).expect("creating the operator-owned parent");
        std::fs::set_permissions(&parent, Permissions::from_mode(0o755))
            .expect("loosening the parent mode");
        ensure_socket_parent(&parent).expect("an existing parent must be accepted");
        let mode = std::fs::metadata(&parent)
            .expect("parent metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o755,
            "the pre-existing parent mode must be left untouched"
        );
        let deep = parent.join("created");
        ensure_socket_parent(&deep).expect("the deeper chain must be created");
        assert_eq!(
            std::fs::metadata(&parent)
                .expect("parent metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "the existing component must not be chmodmed even mid-walk"
        );
        assert_eq!(
            std::fs::metadata(&deep)
                .expect("new component metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the new component must be created 0700"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_symlink_component_is_rejected_and_its_target_untouched() {
        let root = unique_test_root("parent-symlink");
        let target = root.join("target-dir");
        std::fs::create_dir(&target).expect("creating the symlink target");
        std::fs::set_permissions(&target, Permissions::from_mode(0o755))
            .expect("setting the target mode");
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("creating the symlink");
        let deep = link.join("nested");
        let error = ensure_socket_parent(&deep).expect_err("a symlink component must be rejected");
        assert!(
            error.to_string().contains("is a symlink"),
            "the error should name the symlink component: {error:#}"
        );
        assert!(
            !target.join("nested").exists(),
            "the symlink target must be untouched"
        );
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "the symlink itself must be left in place"
        );
        assert_eq!(
            std::fs::metadata(&target)
                .expect("target metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "the target's mode must be untouched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_regular_file_component_is_rejected() {
        let root = unique_test_root("parent-file");
        let blocker = root.join("blocker");
        std::fs::write(&blocker, "not a directory\n").expect("writing the blocking file");
        let deep = blocker.join("nested");
        let error =
            ensure_socket_parent(&deep).expect_err("a non-directory component must be rejected");
        assert!(
            error.to_string().contains("is not a directory"),
            "the error should name the non-directory component: {error:#}"
        );
        assert_eq!(
            std::fs::read_to_string(&blocker).expect("the blocking file should be readable"),
            "not a directory\n",
            "the blocking file must be untouched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn spawn_refuses_a_symlinked_socket_parent() {
        let root = unique_test_root("parent-spawn-symlink");
        let target = root.join("target-dir");
        std::fs::create_dir(&target).expect("creating the symlink target");
        let link = root.join("link");
        std::os::unix::fs::symlink(&target, &link).expect("creating the symlink");
        let socket = link.join("leader.sock");
        let spawn = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
        assert!(
            spawn.is_err(),
            "a spawn through a symlinked parent must be refused"
        );
        assert!(
            !target.join("leader.sock").exists(),
            "nothing may be published through the symlink"
        );
        assert!(
            !target.join("leader.lock").exists(),
            "no lock file may be created through the symlink"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn spawn_refuses_a_regular_file_socket_parent_component() {
        let root = unique_test_root("parent-spawn-file");
        let blocker = root.join("blocker");
        std::fs::write(&blocker, "not a directory\n").expect("writing the blocking file");
        let socket = blocker.join("leader.sock");
        let spawn = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
        assert!(
            spawn.is_err(),
            "a spawn through a regular-file parent component must be refused"
        );
        assert_eq!(
            std::fs::read_to_string(&blocker).expect("the blocking file should be readable"),
            "not a directory\n",
            "the blocking file must be untouched"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn near_limit_long_filename_binds_and_connects() {
        let root = unique_test_root("longfile");
        let socket = long_filename_socket_path(&root, NEAR_LIMIT_PATH_BYTES);
        assert!(socket.as_os_str().len() <= NEAR_LIMIT_PATH_BYTES);
        let factory = recording_factory();
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn near the path-length limit");
        assert_socket_mode_0600(&socket);
        exercise_leader(&socket, &factory).await;
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn near_limit_long_parent_binds_and_connects() {
        let root = unique_test_root("longparent");
        let socket = long_parent_socket_path(&root, NEAR_LIMIT_PATH_BYTES);
        let factory = recording_factory();
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn near the path-length limit");
        assert_socket_mode_0600(&socket);
        exercise_leader(&socket, &factory).await;
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_production_spawn_lifetime_publishes_serves_and_cleans_up() {
        let root = unique_test_root("lifetime");
        let socket = root.join("leader.sock");
        let lock_path = socket.with_extension("lock");
        let factory = recording_factory();
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the production leader should spawn");
        assert_eq!(handle.socket_path(), socket.as_path());
        assert_eq!(handle.lock_path(), lock_path.as_path());
        assert_socket_mode_0600(&socket);
        assert_eq!(
            std::fs::read_to_string(&lock_path)
                .expect("the lock file should be readable")
                .trim()
                .parse::<u32>()
                .expect("the lock file should hold the holder pid"),
            process::id(),
            "the lock file must record the holder pid"
        );
        assert_eq!(
            std::fs::metadata(&lock_path)
                .expect("lock metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the lock file must be forced to 0600"
        );
        exercise_leader(&socket, &factory).await;
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_second_leader_fails_while_the_first_holds_the_lock() {
        let root = unique_test_root("exclusive");
        let socket = root.join("leader.sock");
        let lock_path = socket.with_extension("lock");
        let first_factory = recording_factory();
        let mut first = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            first_factory.clone(),
        )
        .expect("the first leader should spawn");

        let second = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
        assert!(
            second.is_err(),
            "a second leader must fail while the first holds the lock"
        );
        assert!(
            socket.exists(),
            "the failed second leader must not remove the winner's socket"
        );
        assert!(
            lock_path.exists(),
            "the failed second leader must not remove the winner's lock file"
        );
        assert_eq!(
            std::fs::read_to_string(&lock_path)
                .expect("the winner's lock file should be readable")
                .trim()
                .parse::<u32>()
                .expect("the winner's lock file should hold a pid"),
            process::id(),
            "the failed leader must not overwrite the holder pid"
        );

        exercise_leader(&socket, &first_factory).await;
        first.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&first);

        let mut second = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
            .expect("a leader should spawn after the previous one stopped");
        second.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&second);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_stale_lock_file_is_reclaimed_and_forced_to_0600() {
        let root = unique_test_root("stale");
        let socket = root.join("leader.sock");
        let lock_path = socket.with_extension("lock");
        std::fs::write(&lock_path, "999999\n").expect("writing a stale lock file");
        std::fs::set_permissions(&lock_path, Permissions::from_mode(0o644))
            .expect("loosening the stale lock mode");
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
            .expect("a stale lock file must be reclaimable");
        let metadata = std::fs::metadata(&lock_path).expect("the lock file should exist");
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            0o600,
            "the reclaimed lock must be forced to 0600"
        );
        assert_eq!(
            std::fs::read_to_string(&lock_path)
                .expect("the lock file should be readable")
                .trim()
                .parse::<u32>()
                .expect("the lock file should hold a pid"),
            process::id()
        );
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn spawn_refuses_to_replace_a_non_socket_path() {
        let root = unique_test_root("occupied");
        let socket = root.join("leader.sock");
        std::fs::write(&socket, "not a socket\n").expect("writing a blocking file");
        let spawn = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory());
        assert!(spawn.is_err(), "a non-socket path must be refused");
        assert_eq!(
            std::fs::read_to_string(&socket).expect("the blocking file should be untouched"),
            "not a socket\n"
        );
        let lock = socket.with_extension("lock");
        assert!(
            lock.exists(),
            "a failed spawn retains the stable lock inode"
        );
        let probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .expect("opening the released lock");
        probe
            .try_lock()
            .expect("a failed spawn must release its lock");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn dropping_the_handle_unlinks_the_socket_and_leaves_a_reclaimable_lock() {
        let root = unique_test_root("drop");
        let socket = root.join("leader.sock");
        let handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
            .expect("the leader should spawn");
        assert!(socket.exists());
        drop(handle);
        assert!(
            !socket.exists(),
            "dropping the handle must unlink the published socket"
        );
        assert!(
            socket.with_extension("lock").exists(),
            "drop leaves the lock file for the next leader to reclaim"
        );
        let factory = recording_factory();
        let mut next = spawn_with_retry(&socket, factory.clone()).await;
        exercise_leader(&socket, &factory).await;
        next.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&next);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn register_must_precede_registered_and_validate() {
        let root = unique_test_root("register");
        let socket = root.join("leader.sock");
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
            .expect("the leader should spawn");

        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
            match next_server_frame(&mut reader).await {
                Some(ServerEnvelope::Error { code, .. }) => {
                    assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
                }
                other => panic!("expected an error envelope, got {other:?}"),
            }
            assert!(
                next_server_frame(&mut reader).await.is_none(),
                "the leader must close after a protocol violation"
            );
        }
        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(
                &mut writer,
                &ClientEnvelope::Acp {
                    payload: json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}).to_string(),
                },
            )
            .await;
            match next_server_frame(&mut reader).await {
                Some(ServerEnvelope::Error { code, .. }) => {
                    assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
                }
                other => panic!("expected an error envelope, got {other:?}"),
            }
            assert!(next_server_frame(&mut reader).await.is_none());
        }
        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(
                &mut writer,
                &ClientEnvelope::Register {
                    client_type: "grok-pager".to_string(),
                    mode: RegisterMode::Headless,
                    capabilities: test_capabilities(),
                },
            )
            .await;
            match next_server_frame(&mut reader).await {
                Some(ServerEnvelope::Registered { .. }) => {}
                other => panic!("expected registered, got {other:?}"),
            }
        }
        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(
                &mut writer,
                &ClientEnvelope::Register {
                    client_type: "   ".to_string(),
                    mode: RegisterMode::Stdio,
                    capabilities: test_capabilities(),
                },
            )
            .await;
            match next_server_frame(&mut reader).await {
                Some(ServerEnvelope::Error { code, .. }) => {
                    assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
                }
                other => panic!("expected an error envelope, got {other:?}"),
            }
            assert!(next_server_frame(&mut reader).await.is_none());
        }
        {
            let (reader, writer, _client_id) = register(&socket).await;
            drop((reader, writer));
        }

        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_second_register_is_rejected() {
        let root = unique_test_root("reregister");
        let socket = root.join("leader.sock");
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
            .expect("the leader should spawn");
        let (mut reader, mut writer, _client_id) = register(&socket).await;
        write_client_frame(&mut writer, &register_envelope()).await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Error { code, .. }) => {
                assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
            }
            other => panic!("expected an error envelope, got {other:?}"),
        }
        assert!(
            next_server_frame(&mut reader).await.is_none(),
            "the leader must close after a second register"
        );
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn registration_captures_client_capabilities() {
        let root = unique_test_root("caps");
        let socket = root.join("leader.sock");
        let factory = recording_factory();
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn");
        let (mut reader, mut writer, _client_id) = register(&socket).await;
        write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Pong) => {}
            other => panic!("expected pong, got {other:?}"),
        }
        let constructed = factory.constructed();
        let delegate = constructed
            .first()
            .expect("registration must have constructed a delegate");
        assert_eq!(
            delegate
                .capabilities
                .lock()
                .expect("capability log")
                .clone(),
            Some((true, false, false, Some("grok-pager-test".to_string()))),
            "yolo_mode/auto_mode/terminal and the client version must reach the \
             per-connection delegate at construction time"
        );
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn control_commands_answer_method_not_found() {
        let root = unique_test_root("control");
        let socket = root.join("leader.sock");
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
            .expect("the leader should spawn");
        let (mut reader, mut writer, _client_id) = register(&socket).await;
        write_client_frame(
            &mut writer,
            &ClientEnvelope::Control {
                request_id: "req-1".to_string(),
                command: json!({"type": "relaunch"}),
            },
        )
        .await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Error { code, message }) => {
                assert_eq!(code, ENVELOPE_ERROR_METHOD_NOT_FOUND);
                assert!(
                    message.contains("req-1"),
                    "the error should name the control request"
                );
            }
            other => panic!("expected an error envelope, got {other:?}"),
        }
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn deferred_acp_pushes_arrive_after_dispatch_returns() {
        let root = unique_test_root("deferred");
        let socket = root.join("leader.sock");
        let factory = Arc::new(RecordingFactory {
            late_push: true,
            ..Default::default()
        });
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn");
        let (mut reader, mut writer, _client_id) = register(&socket).await;
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/prompt",
            "params": {"sessionId": "s"},
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
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Acp { payload }) => {
                assert!(payload.contains("deferred"), "got {payload}");
            }
            other => panic!("expected the deferred push, got {other:?}"),
        }
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn two_registrations_receive_distinct_delegate_instances() {
        let root = unique_test_root("per-connection");
        let socket = root.join("leader.sock");
        let factory = recording_factory();
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn");

        let (reader_a, writer_a, client_id_a) = register(&socket).await;
        let (reader_b, writer_b, client_id_b) = register(&socket).await;
        assert_ne!(
            client_id_a, client_id_b,
            "two registrations must receive distinct client ids"
        );
        let connections = (reader_a, writer_a, reader_b, writer_b);

        let delegates = factory.constructed();
        assert_eq!(
            delegates.len(),
            2,
            "each registered connection must construct exactly one delegate"
        );
        let instance_ids: Vec<u64> = delegates.iter().map(|d| d.instance_id).collect();
        assert_ne!(
            instance_ids[0], instance_ids[1],
            "two registrations must receive distinct delegate instances"
        );
        assert_eq!(
            factory.observed_client_ids(),
            vec![client_id_a, client_id_b],
            "the factory must see each connection's generated client id"
        );
        assert!(
            delegates[0].recorded_client_ids().contains(&client_id_a),
            "delegate A must carry client A's id"
        );
        assert!(
            delegates[1].recorded_client_ids().contains(&client_id_b),
            "delegate B must carry client B's id"
        );

        drop(connections);
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn failed_registration_constructs_zero_delegates() {
        let root = unique_test_root("failed-registration");
        let socket = root.join("leader.sock");
        let factory = recording_factory();
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn");

        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(
                &mut writer,
                &ClientEnvelope::Register {
                    client_type: "   ".to_string(),
                    mode: RegisterMode::Stdio,
                    capabilities: test_capabilities(),
                },
            )
            .await;
            match next_server_frame(&mut reader).await {
                Some(ServerEnvelope::Error { code, .. }) => {
                    assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST)
                }
                other => panic!("expected an error envelope, got {other:?}"),
            }
            assert!(next_server_frame(&mut reader).await.is_none());
        }
        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
            assert!(next_server_frame(&mut reader).await.is_some());
            assert!(next_server_frame(&mut reader).await.is_none());
        }

        assert_eq!(
            factory.constructed_count(),
            0,
            "failed registrations must construct zero delegates"
        );
        assert!(
            factory.observed_client_ids().is_empty(),
            "the factory must never be invoked for a failed registration"
        );

        let (_reader, _writer, _client_id) = register(&socket).await;
        assert_eq!(factory.constructed_count(), 1);

        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_failing_factory_closes_the_connection_and_constructs_no_delegate() {
        let root = unique_test_root("failing-factory");
        let socket = root.join("leader.sock");
        let factory = Arc::new(FailingFactory::default());
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn");

        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(&mut writer, &register_envelope()).await;
            assert!(
                next_server_frame(&mut reader).await.is_none(),
                "the leader must close the connection when the factory fails"
            );
        }
        assert_eq!(
            factory.attempts.load(Ordering::SeqCst),
            1,
            "the factory must have been invoked exactly once"
        );

        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn disconnecting_b_notifies_only_b_and_a_still_serves() {
        let root = unique_test_root("isolation");
        let socket = root.join("leader.sock");
        let factory = recording_factory();
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory.clone())
            .expect("the leader should spawn");

        let (mut reader_a, mut writer_a, _client_id_a) = register(&socket).await;
        let (mut reader_b, mut writer_b, _client_id_b) = register(&socket).await;

        let delegates = factory.constructed();
        assert_eq!(delegates.len(), 2);
        let delegate_a = delegates[0].clone();
        let delegate_b = delegates[1].clone();

        write_client_frame(&mut writer_b, &ClientEnvelope::Disconnect).await;
        assert!(
            next_server_frame(&mut reader_b).await.is_none(),
            "the leader should close connection B after its disconnect"
        );
        assert_eq!(
            delegate_b.disconnect_count(),
            1,
            "connection B's on_disconnect must run exactly once"
        );
        assert_eq!(
            delegate_a.disconnect_count(),
            0,
            "connection A's on_disconnect must not run for B's disconnect"
        );

        write_client_frame(&mut writer_a, &ClientEnvelope::Ping).await;
        match next_server_frame(&mut reader_a).await {
            Some(ServerEnvelope::Pong) => {}
            other => panic!("expected pong for A after B disconnected, got {other:?}"),
        }
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "initialize",
            "params": {},
        })
        .to_string();
        write_client_frame(
            &mut writer_a,
            &ClientEnvelope::Acp {
                payload: payload.clone(),
            },
        )
        .await;
        match next_server_frame(&mut reader_a).await {
            Some(ServerEnvelope::Acp { payload: echoed }) => assert_eq!(echoed, payload),
            other => panic!("expected an acp echo for A, got {other:?}"),
        }
        let payloads_b = delegate_b.payloads.lock().expect("payload log").clone();
        assert!(
            payloads_b.is_empty(),
            "B's delegate must never see A's ACP dispatch"
        );
        assert_eq!(
            delegate_a.disconnect_count(),
            0,
            "A's delegate must still be connected after serving A"
        );

        drop((reader_a, writer_a));
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn default_shutdown_waits_for_connection_on_disconnect_cleanup() {
        let root = unique_test_root("shutdown-disconnect-cleanup");
        let socket = root.join("leader.sock");
        let delegate = GatedDisconnectDelegate::new();
        let factory: Arc<dyn AcpDelegateFactory> = Arc::new({
            let delegate = delegate.clone();
            move |_client_id: u64, _registration: &Registration| {
                Ok(delegate.clone() as Arc<dyn AcpDelegate>)
            }
        });
        let handle = spawn_leader(LeaderServerConfig::new(socket.clone()), factory)
            .expect("the leader should spawn");
        let (_reader, _writer, _client_id) = register(&socket).await;

        let mut shutdown = tokio::spawn(async move {
            let mut handle = handle;
            let result = handle.shutdown().await;
            (handle, result)
        });
        timeout(TEST_TIMEOUT, delegate.started.notified())
            .await
            .expect("the connection should enter on_disconnect");
        assert!(
            timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err(),
            "LeaderHandle::shutdown must wait while on_disconnect is still running"
        );

        delegate.release.notify_one();
        let (handle, result) = timeout(TEST_TIMEOUT, shutdown)
            .await
            .expect("leader shutdown should finish after disconnect cleanup")
            .expect("shutdown task should join");
        result.expect("leader shutdown should succeed");
        assert!(
            delegate.completed.load(Ordering::SeqCst),
            "shutdown may return only after on_disconnect completed"
        );
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn shutdown_announces_the_stop_to_live_connections() {
        let root = unique_test_root("shutdown-frames");
        let socket = root.join("leader.sock");
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), recording_factory())
            .expect("the leader should spawn");
        let (mut reader, _writer, _client_id) = register(&socket).await;
        handle.shutdown().await.expect("clean shutdown");
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::ShuttingDown { reason, delay_ms }) => {
                assert_eq!(reason, ShutdownReason::Manual);
                assert_eq!(delay_ms, 0);
            }
            other => panic!("expected shutting_down, got {other:?}"),
        }
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Shutdown) => {}
            other => panic!("expected shutdown, got {other:?}"),
        }
        assert!(
            next_server_frame(&mut reader).await.is_none(),
            "the leader should close the connection after shutdown"
        );
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }
}
