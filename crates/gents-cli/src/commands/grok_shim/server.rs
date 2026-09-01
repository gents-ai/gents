//! Grok shim leader server: the Unix-domain-socket leader that stock Grok
//! attaches to as its pager client.
//!
//! Gents binds the socket; the pager client connects to it. Ownership and
//! lifecycle contract (every rule below is enforced in this file and exercised
//! by the tests at the bottom):
//!
//! * Election is exclusive. [`spawn_leader`] acquires the sibling
//!   extension-swapped lock at `socket_path.with_extension("lock")` — the same
//!   extension swap a stock Grok leader performs, so the shim and the closed
//!   source leader can never both own one socket. The lock file is opened with
//!   `O_NOFOLLOW` so a symlink planted at the lock path cannot redirect the
//!   open, forced to `0600`, and locked with a *nonblocking exclusive* lock
//!   before anything is published; the holder PID is written for diagnostics.
//!   A second leader on the same socket path fails fast and never removes or
//!   replaces the winner's lock or socket.
//! * The open lock guard is moved into the accept-loop future, so the lock is
//!   held for exactly the lifetime of the spawned listener task, not merely
//!   for the lifetime of [`LeaderHandle`].
//! * The socket is published atomically: the listener binds inside a private
//!   `0700` short same-device staging ancestor, the socket is forced to
//!   `0600` while it is still unreachable inside the staging directory, and
//!   only then is it `rename(2)`d onto the published path. Binding therefore
//!   never depends on the length of the published path, so both a long parent
//!   and a long filename can really bind and connect, and a pager client only
//!   ever observes a finished `0600` socket or no socket at all.
//! * Registration order is enforced: the first frame on a connection must be a
//!   valid `register` envelope; `registered` is written only after it
//!   validates, with `leader_binary_version = gents-<CARGO_PKG_VERSION>`.
//! * `ping` is answered with `pong`, `acp` frames are dispatched to the
//!   [`AcpDelegate`] with a connection-scoped outbound handle, unsupported
//!   `control` commands answer a method-not-found error envelope, and
//!   `disconnect` (or EOF) triggers connection cleanup and the delegate's
//!   disconnect notification.
//! * On a clean stop the accept loop announces `shutting_down` plus `shutdown`
//!   to every live connection, releases the leader lock, and removes the
//!   socket and lock file. [`LeaderHandle::shutdown`] awaits that clean stop.
//!   Dropping the handle without shutting down is the documented emergency
//!   path: it aborts the listener task and unlinks the published socket, and
//!   deliberately leaves the lock file for the next leader to reclaim, because
//!   unlinking a lock file the dropper may no longer hold would let a new
//!   leader lose exclusivity.
//!
//! Two deliberate adjustments to the leader contract, both forced by the
//! "no Cargo dependency change" rule for this shim:
//!
//! * The nonblocking exclusive lock is taken with `std::fs::File::try_lock`.
//!   On Unix the standard library implements that API with `flock(2)`
//!   (`LOCK_EX | LOCK_NB`) on the open file description, which is exactly the
//!   primitive the leader contract requires. `gents-cli` has no `libc` or
//!   `nix` dependency and this slice may not add one, so the shim calls the
//!   standard-library API instead of `libc::flock` directly.
//! * `O_NOFOLLOW` is passed to `OpenOptions::custom_flags` from a per-target
//!   constant declared in this file with the same ABI value `libc`/`nix` use.
//!
//! Expected `super::protocol` surface (owned by the wire-codec slice;
//! convergence reconciles any naming drift):
//!
//! * `ClientEnvelope` enum, serde-tagged `"type"`, snake_case:
//!   `Register { client_type: String, mode: String, capabilities:
//!   ClientCapabilities }`, `Acp { payload: String }`,
//!   `Control { request_id: String, command: serde_json::Value }`, `Ping`,
//!   `Disconnect`.
//! * `ServerEnvelope` enum: `Registered { client_id: u64, ready: bool,
//!   leader_protocol_version: u32, leader_binary_version: String,
//!   leader_capabilities: LeaderCapabilities }`, `Acp { payload: String }`,
//!   `Pong`, `Error { code: i32, message: String }`,
//!   `ShuttingDown { reason: String, delay_ms: u64 }`, `Shutdown`.
//! * `ClientCapabilities { yolo_mode, auto_mode, default_model, client_version,
//!   code_nav_enabled, terminal, fs_read, fs_write, status_line }` and
//!   `LeaderCapabilities { control_v1, runtime_cpu_profile, profile_formats,
//!   workspace_exposure, relaunch_v1 }`, both `Clone + Debug`.
//! * `pub const LEADER_PROTOCOL_VERSION: u32 = 1;`
//! * `async fn read_frame<R: AsyncRead + Unpin, E: DeserializeOwned>(reader:
//!   &mut R) -> anyhow::Result<Option<E>>` — `Ok(None)` is a clean EOF before
//!   any payload byte; truncation, oversize, and invalid JSON are errors.
//! * `async fn write_frame<W: AsyncWrite + Unpin, E: Serialize>(writer: &mut W,
//!   envelope: &E) -> anyhow::Result<()>` — four-byte big-endian length
//!   prefix.

use std::fs::{File, OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::future::BoxFuture;
use serde_json::Value;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

use super::protocol::{
    read_frame, write_frame, ClientCapabilities, ClientEnvelope, LeaderCapabilities,
    ServerEnvelope, LEADER_PROTOCOL_VERSION,
};

/// Tracing target for every log line this module emits.
const LOG_TARGET: &str = "gents_cli::commands::grok_shim::server";

/// Reason string announced in `shutting_down` for a stop requested through
/// [`LeaderHandle::shutdown`].
const SHUTDOWN_REASON_MANUAL: &str = "manual";

/// Envelope error code for a frame that violates the leader protocol.
const ENVELOPE_ERROR_INVALID_REQUEST: i32 = -32600;

/// Envelope error code for a leader method this shim does not implement.
const ENVELOPE_ERROR_METHOD_NOT_FOUND: i32 = -32601;

/// JSON-RPC internal-error code used when ACP dispatch itself fails.
const JSONRPC_INTERNAL_ERROR: i64 = -32603;

/// Mode forced on the lock file and on the published socket.
const PRIVATE_FILE_MODE: u32 = 0o600;

/// Mode of the private staging ancestor directory.
const STAGING_DIR_MODE: u32 = 0o700;

/// Name of the socket *inside* the staging directory. A single character keeps
/// the bind path short even when the published path is near the `sun_path`
/// limit, which is the whole point of staging.
const STAGED_SOCKET_NAME: &str = "s";

/// Prefix of the private staging directory; the eight random hex characters
/// that follow keep the entire staging bind path short.
const STAGING_DIR_PREFIX: &str = ".gents-grok-";

/// Naming attempts per staging ancestor before falling back to a deeper one.
const STAGING_ATTEMPTS: usize = 8;

/// Upper bound on any shutdown grace, so a large configured `shutdown_delay_ms`
/// cannot stall a clean stop.
const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Published socket path length exercised by the near-limit tests. `sun_path`
/// is 104 bytes on macOS and 108 on Linux, so 100 bytes of path is genuinely
/// near the limit while remaining connectable on every supported target.
const NEAR_LIMIT_PATH_BYTES: usize = 100;

/// `O_NOFOLLOW` for `OpenOptions::custom_flags`.
///
/// The values are the OS ABI constants that `libc` and `nix` define per target.
/// Unsupported Unix targets fail the build rather than silently opening a lock
/// path through a symlink.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("the grok shim leader lock needs this target's O_NOFOLLOW value; add it here");
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0x0000_8000;
#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x0000_0040;

// ---------------------------------------------------------------------------
// ACP delegate seam
// ---------------------------------------------------------------------------

/// Connection-scoped outbound handle for ACP payloads.
///
/// The leader owns frame writing; the delegate only hands it finished JSON-RPC
/// lines. The handle is cheap to clone, so a delegate can keep one for a
/// deferred response (for example a `session/prompt` result that must wait for
/// turn terminalization) and push notifications while a turn streams. Sending
/// after the connection closed is an error the caller may ignore.
#[derive(Clone)]
pub(crate) struct AcpOutbound {
    frames: mpsc::UnboundedSender<ServerEnvelope>,
}

impl AcpOutbound {
    /// Queue one ACP JSON-RPC line for the pager client.
    pub(crate) fn send(&self, payload: impl Into<String>) -> Result<()> {
        self.frames
            .send(ServerEnvelope::Acp {
                payload: payload.into(),
            })
            .map_err(|_| anyhow!("the grok shim leader connection is closed"))
    }
}

/// The ACP behavior behind the leader, implemented by `acp.rs`.
///
/// The methods are object-safe boxed futures because the leader stores the
/// delegate as `Arc<dyn AcpDelegate>` and dispatches each ACP frame on its own
/// task: a `session/prompt` handler must be able to run to terminalization
/// while the connection keeps reading (a `session/cancel` frame must not wait
/// behind the prompt it cancels).
pub(crate) trait AcpDelegate: Send + Sync + 'static {
    /// Handle one inbound ACP JSON-RPC line. Responses and notifications are
    /// pushed through `outbound`; the leader never interprets ACP payloads.
    fn handle_acp<'a>(
        &'a self,
        payload: &'a str,
        outbound: AcpOutbound,
    ) -> BoxFuture<'a, Result<()>>;

    /// Observe the registering client's capabilities. The ACP service derives
    /// the `yoloMode` / `autoMode` / `clientTerminal` injection for
    /// `session/new` from them.
    fn on_client_capabilities<'a>(
        &'a self,
        capabilities: &'a ClientCapabilities,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// The connection went away (disconnect, EOF, protocol violation, or
    /// leader shutdown). The ACP service drains and interrupts its
    /// connection-scoped pending turns.
    fn on_disconnect(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

// ---------------------------------------------------------------------------
// Configuration and handle
// ---------------------------------------------------------------------------

/// Configuration for one spawned leader.
#[derive(Debug, Clone)]
pub(crate) struct LeaderServerConfig {
    /// Filesystem path of the published leader socket.
    pub(crate) socket_path: PathBuf,
    /// Delay announced in the `shutting_down` frame before the stop, and the
    /// bound the accept loop waits for live connections to finish. Zero skips
    /// the wait entirely.
    pub(crate) shutdown_delay_ms: u64,
}

impl LeaderServerConfig {
    pub(crate) fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            shutdown_delay_ms: 0,
        }
    }

    /// The sibling extension-swapped lock path for this socket.
    pub(crate) fn lock_path(&self) -> PathBuf {
        self.socket_path.with_extension("lock")
    }

    pub(crate) fn with_shutdown_delay_ms(mut self, delay_ms: u64) -> Self {
        self.shutdown_delay_ms = delay_ms;
        self
    }
}

/// Handle to one spawned leader. Owns shutdown and the listener task.
pub(crate) struct LeaderHandle {
    socket_path: PathBuf,
    lock_path: PathBuf,
    shutdown_tx: watch::Sender<bool>,
    task: Option<JoinHandle<Result<()>>>,
}

impl LeaderHandle {
    /// Published socket path.
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Leader lock path (the socket path with its extension swapped to
    /// `lock`).
    pub(crate) fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Request a clean stop and wait for the listener task to finish it.
    ///
    /// On return the socket and lock file have been removed and the exclusive
    /// leader lock has been released, so a new leader may take the socket.
    pub(crate) async fn shutdown(&mut self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        self.join().await
    }

    /// Wait for the listener task to finish without requesting a stop.
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
        // Emergency cleanup only. The published socket is unlinked
        // synchronously so no pager client can keep connecting to a dead
        // leader. The lock file is deliberately left in place: once the
        // aborted task drops the guard the next `spawn_leader` reclaims it
        // safely, whereas unlinking a lock file this dropper may no longer
        // hold would let a concurrently starting leader lose exclusivity.
        // `shutdown` is the clean path that also removes the lock file.
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

/// Spawn the production leader for `config`.
///
/// Election, stale-socket removal, and socket publication all happen before
/// the accept-loop task is spawned, and the open lock guard is moved into that
/// task so the exclusive lock is held for the actual listener lifetime.
///
/// Must be called from within a Tokio runtime: the bound listener registers
/// with the runtime reactor, and the accept loop is a spawned task.
pub(crate) fn spawn_leader(
    config: LeaderServerConfig,
    delegate: Arc<dyn AcpDelegate>,
) -> Result<LeaderHandle> {
    let socket_path = config.socket_path.clone();
    if socket_path
        .file_name()
        .is_none_or(|name| name.is_empty())
    {
        bail!(
            "the grok shim leader socket path {} must name a socket file",
            socket_path.display()
        );
    }

    // 1. Exclusive election. Every later step happens under this lock, and a
    //    loser never touches the winner's lock or socket.
    let lock = LeaderLock::acquire(&socket_path)?;

    // 2. Remove a stale socket left by a crashed leader. Only reachable while
    //    we hold the exclusive lock, so this can never delete a live leader's
    //    socket.
    if let Err(error) = remove_stale_socket(&socket_path) {
        lock.release();
        return Err(error);
    }

    // 3. Publish the socket atomically from a private staging ancestor.
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
        lock = %config.lock_path().display(),
        "grok shim leader listening for the pager client"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (lifecycle_tx, _lifecycle_rx) = broadcast::channel::<ServerEnvelope>(16);
    let lock_path = config.lock_path();
    let shutdown_delay_ms = config.shutdown_delay_ms;
    let task = tokio::spawn(accept_loop(
        listener,
        // The open guard moves into the accept-loop future: the exclusive
        // lock is held for exactly the spawned listener lifetime.
        lock,
        socket_path.clone(),
        delegate,
        lifecycle_tx,
        shutdown_rx,
        shutdown_delay_ms,
    ));

    Ok(LeaderHandle {
        socket_path,
        lock_path,
        shutdown_tx,
        task: Some(task),
    })
}

// ---------------------------------------------------------------------------
// Accept loop
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: UnixListener,
    lock: LeaderLock,
    socket_path: PathBuf,
    delegate: Arc<dyn AcpDelegate>,
    lifecycle: broadcast::Sender<ServerEnvelope>,
    mut shutdown: watch::Receiver<bool>,
    shutdown_delay_ms: u64,
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
                            delegate.clone(),
                            lifecycle_rx,
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            %error,
                            "grok shim leader failed to accept a pager connection"
                        );
                        // Back off so a persistent accept error cannot hot-spin.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    // Announce the stop to every live connection, then stop accepting.
    let _ = lifecycle.send(ServerEnvelope::ShuttingDown {
        reason: SHUTDOWN_REASON_MANUAL.to_string(),
        delay_ms: shutdown_delay_ms,
    });
    let _ = lifecycle.send(ServerEnvelope::Shutdown);

    if shutdown_delay_ms > 0 {
        let grace = Duration::from_millis(shutdown_delay_ms).min(MAX_SHUTDOWN_GRACE);
        let drain = async {
            while connections.join_next().await.is_some() {}
        };
        if tokio::time::timeout(grace, drain).await.is_err() {
            tracing::debug!(
                target: LOG_TARGET,
                "grok shim leader shutdown grace elapsed with connections still open"
            );
        }
    } else {
        // Connections observe the lifecycle frames (or the closed lifecycle
        // channel when this task returns) and clean themselves up.
        connections.detach_all();
    }

    drop(listener);

    // Clean stop: remove the published socket, then release the leader lock
    // (which removes the lock file while the exclusive lock is still held).
    remove_published_socket(&socket_path);
    lock.release();
    tracing::info!(
        target: LOG_TARGET,
        socket = %socket_path.display(),
        "grok shim leader stopped cleanly"
    );
    Ok(())
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

// ---------------------------------------------------------------------------
// Per-connection handling
// ---------------------------------------------------------------------------

/// A validated `register` frame.
struct Registration {
    client_type: String,
    mode: String,
    capabilities: ClientCapabilities,
}

async fn handle_connection(
    stream: UnixStream,
    client_id: u64,
    delegate: Arc<dyn AcpDelegate>,
    mut lifecycle: broadcast::Receiver<ServerEnvelope>,
) {
    let (mut reader, writer) = stream.into_split();
    let (frames_tx, mut frames_rx) = mpsc::unbounded_channel::<ServerEnvelope>();
    let writer_task = tokio::spawn(async move {
        let mut writer = writer;
        while let Some(envelope) = frames_rx.recv().await {
            if let Err(error) = write_frame(&mut writer, &envelope).await {
                tracing::debug!(
                    target: LOG_TARGET,
                    %error,
                    "grok shim leader stopped writing to a pager connection"
                );
                break;
            }
        }
    });

    // Phase 1: the first frame must be a valid register; `registered` is only
    // written after it validates.
    let registration = match register_client(&mut reader, &frames_tx, &mut lifecycle).await {
        Ok(registration) => registration,
        Err(()) => {
            // register_client already logged the cause and, where the protocol
            // demands it, wrote the error envelope.
            delegate.on_disconnect().await;
            drop(frames_tx);
            let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
            return;
        }
    };

    let _ = frames_tx.send(ServerEnvelope::Registered {
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
        mode = %registration.mode,
        yolo_mode = registration.capabilities.yolo_mode,
        auto_mode = registration.capabilities.auto_mode,
        terminal = registration.capabilities.terminal,
        "grok shim leader registered a pager client"
    );
    delegate
        .on_client_capabilities(&registration.capabilities)
        .await;

    // Phase 2: serve frames. Each ACP frame is dispatched on its own task so
    // a long-running prompt cannot block reading the cancel that stops it.
    let outbound = AcpOutbound {
        frames: frames_tx.clone(),
    };
    let mut acp_tasks: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            lifecycle_frame = lifecycle.recv() => {
                match lifecycle_frame {
                    Ok(envelope) => {
                        // Forward `shutting_down` and `shutdown`; the closed
                        // channel below ends the loop.
                        let _ = frames_tx.send(envelope);
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
            frame = read_frame::<_, ClientEnvelope>(&mut reader) => {
                match frame {
                    Ok(Some(ClientEnvelope::Ping)) => {
                        let _ = frames_tx.send(ServerEnvelope::Pong);
                    }
                    Ok(Some(ClientEnvelope::Acp { payload })) => {
                        spawn_acp_dispatch(
                            &mut acp_tasks,
                            delegate.clone(),
                            outbound.clone(),
                            payload,
                        );
                    }
                    Ok(Some(ClientEnvelope::Control { request_id, command })) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            client_id,
                            request_id = %request_id,
                            command = %command,
                            "grok shim leader received an unsupported control command"
                        );
                        let _ = frames_tx.send(ServerEnvelope::Error {
                            code: ENVELOPE_ERROR_METHOD_NOT_FOUND,
                            message: format!(
                                "the Gents leader shim does not implement control commands \
                                 (request {request_id:?}); leader_capabilities.control_v1 is false"
                            ),
                        });
                    }
                    Ok(Some(ClientEnvelope::Register { .. })) => {
                        protocol_violation(
                            &frames_tx,
                            "register is only valid as the first frame on a connection",
                        );
                        break;
                    }
                    Ok(Some(ClientEnvelope::Disconnect)) => {
                        tracing::debug!(
                            target: LOG_TARGET,
                            client_id,
                            "grok shim leader pager client disconnected"
                        );
                        break;
                    }
                    Ok(None) => break,
                    Err(error) => {
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
                }
            }
        }
    }

    // Connection cleanup: drain the delegate's connection-scoped state first
    // so in-flight handlers observe the drained turn table, then let the
    // queued frames flush before the writer is dropped.
    delegate.on_disconnect().await;
    let drain = async {
        while acp_tasks.join_next().await.is_some() {}
    };
    if tokio::time::timeout(MAX_SHUTDOWN_GRACE, drain).await.is_err() {
        tracing::warn!(
            target: LOG_TARGET,
            client_id,
            "grok shim leader closed a connection with ACP dispatch still running"
        );
    }
    drop(frames_tx);
    let _ = tokio::time::timeout(MAX_SHUTDOWN_GRACE, writer_task).await;
}

/// Read frames until a valid `register` arrives. `Err(())` means the
/// connection must close; the error envelope (when the protocol requires one)
/// has already been queued.
async fn register_client(
    reader: &mut OwnedReadHalf,
    frames: &mpsc::UnboundedSender<ServerEnvelope>,
    lifecycle: &mut broadcast::Receiver<ServerEnvelope>,
) -> std::result::Result<Registration, ()> {
    loop {
        let frame = tokio::select! {
            lifecycle_frame = lifecycle.recv() => {
                let _ = lifecycle_frame;
                // The leader is stopping before this client registered.
                return Err(());
            }
            frame = read_frame::<_, ClientEnvelope>(reader) => frame,
        };
        match frame {
            Ok(Some(ClientEnvelope::Register {
                client_type,
                mode,
                capabilities,
            })) => {
                if let Err(reason) = validate_register(&client_type, &mode) {
                    tracing::warn!(
                        target: LOG_TARGET,
                        client_type = %client_type,
                        mode = %mode,
                        reason = %format!("{reason:#}"),
                        "grok shim leader rejected an invalid register frame"
                    );
                    let _ = frames.send(ServerEnvelope::Error {
                        code: ENVELOPE_ERROR_INVALID_REQUEST,
                        message: format!("{reason:#}"),
                    });
                    return Err(());
                }
                return Ok(Registration {
                    client_type,
                    mode,
                    capabilities,
                });
            }
            Ok(Some(_)) => {
                protocol_violation(
                    frames,
                    "the first frame on a leader connection must be register",
                );
                return Err(());
            }
            Ok(None) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    "grok shim leader connection closed before register"
                );
                return Err(());
            }
            Err(error) => {
                protocol_violation(
                    frames,
                    &format!("undecodable leader frame before register: {error}"),
                );
                return Err(());
            }
        }
    }
}

fn validate_register(client_type: &str, mode: &str) -> Result<()> {
    if client_type.trim().is_empty() {
        bail!("register requires a non-empty client_type");
    }
    if !matches!(mode, "stdio" | "headless") {
        bail!(
            "register mode {mode:?} is not one of \"stdio\" or \"headless\""
        );
    }
    Ok(())
}

fn protocol_violation(frames: &mpsc::UnboundedSender<ServerEnvelope>, message: &str) {
    tracing::warn!(target: LOG_TARGET, message, "grok shim leader protocol violation");
    let _ = frames.send(ServerEnvelope::Error {
        code: ENVELOPE_ERROR_INVALID_REQUEST,
        message: message.to_string(),
    });
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
                if let Err(error) = outbound.send(response) {
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

/// Build a JSON-RPC internal-error response for a failed request. Returns
/// `None` for notifications and undecodable payloads, which expect no answer.
fn internal_error_response(payload: &str) -> Option<String> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let id = value.get("id")?;
    if value.get("method").is_none() {
        return None;
    }
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
    // The shim implements none of the optional leader extensions; the pager
    // reads these flags to decide what it may send.
    LeaderCapabilities {
        control_v1: false,
        runtime_cpu_profile: false,
        profile_formats: Vec::new(),
        workspace_exposure: false,
        relaunch_v1: false,
    }
}

// ---------------------------------------------------------------------------
// Exclusive leader lock
// ---------------------------------------------------------------------------

/// The exclusive sibling leader lock.
///
/// The lock file is the published socket path with its extension swapped to
/// `lock`. It is opened with `O_NOFOLLOW`, forced to `0600`, and locked with a
/// nonblocking exclusive lock (`File::try_lock`, i.e. `flock(2)` with
/// `LOCK_EX | LOCK_NB` on Unix, on the open file description). The holder PID
/// is written for diagnostics. The open `File` is the guard: the lock lives
/// exactly as long as it does, which is why [`spawn_leader`] moves the guard
/// into the accept-loop future.
struct LeaderLock {
    path: PathBuf,
    file: File,
}

impl LeaderLock {
    fn acquire(socket_path: &Path) -> Result<Self> {
        let path = socket_path.with_extension("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(O_NOFOLLOW)
            .open(&path)
            .with_context(|| {
                format!("opening the grok shim leader lock {}", path.display())
            })?;
        force_private_mode(&file, &path)?;
        if let Err(error) = file.try_lock() {
            if error.kind() == std::io::ErrorKind::WouldBlock {
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
        let mut file = file;
        write_holder_pid(&mut file).with_context(|| {
            format!("recording the leader pid in {}", path.display())
        })?;
        Ok(Self { path, file })
    }

    /// Release the lock: remove the lock file while the exclusive lock is
    /// still held, but only while the path still names the inode we locked.
    /// Dropping the guard afterwards releases the exclusive lock.
    fn release(self) {
        let locked = file_identity(&self.file);
        let on_path = std::fs::metadata(&self.path)
            .ok()
            .and_then(|metadata| Some((metadata.dev(), metadata.ino())));
        match (locked, on_path) {
            (Some(locked), Some(on_path)) if locked == on_path => {
                match std::fs::remove_file(&self.path) {
                    Ok(()) => tracing::debug!(
                        target: LOG_TARGET,
                        lock = %self.path.display(),
                        "released the grok shim leader lock"
                    ),
                    Err(error) => tracing::warn!(
                        target: LOG_TARGET,
                        %error,
                        lock = %self.path.display(),
                        "failed to remove the grok shim leader lock on clean stop"
                    ),
                }
            }
            _ => {
                tracing::warn!(
                    target: LOG_TARGET,
                    lock = %self.path.display(),
                    "the grok shim leader lock path no longer names the locked inode; leaving it in place"
                );
            }
        }
        drop(self.file);
    }
}

/// Force `0600` on the open lock file and verify it through the descriptor.
///
/// The mode passed to `open(2)` is masked by the process umask and ignored
/// entirely when the file already exists, so the mode is set on the open file
/// and then re-read through the same descriptor. Verifying through the
/// descriptor (not the path) fails closed if anything swapped the path.
fn force_private_mode(file: &File, path: &Path) -> Result<()> {
    let current = file
        .metadata()
        .with_context(|| {
            format!("reading the mode of the grok shim leader lock {}", path.display())
        })?
        .permissions()
        .mode();
    if current & 0o777 == PRIVATE_FILE_MODE {
        return Ok(());
    }
    file.set_permissions(Permissions::from_mode(PRIVATE_FILE_MODE))
        .with_context(|| {
            format!("forcing mode 0600 on the grok shim leader lock {}", path.display())
        })?;
    let forced = file
        .metadata()
        .with_context(|| {
            format!("re-reading the mode of the grok shim leader lock {}", path.display())
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

/// PID recorded in the lock file, for the "another leader holds the lock"
/// diagnostic. Path-based read: it is only used in a log message.
fn read_holder_pid(lock_path: &Path) -> Option<u32> {
    std::fs::read_to_string(lock_path)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_holder_pid(file: &mut File) -> Result<()> {
    file.set_len(0)
        .with_context(|| "truncating the grok shim leader lock".to_string())?;
    file.write_all(format!("{}\n", process::id()).as_bytes())
        .with_context(|| "writing the leader pid into the lock file".to_string())?;
    Ok(())
}

/// Remove a stale socket left behind by a crashed leader. Only called while
/// the exclusive lock is held, so this can never delete a live leader's
/// socket. Anything that is not a plain socket file is refused.
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
                format!("removing the stale grok shim socket {}", socket_path.display())
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
                format!("inspecting the grok shim socket path {}", socket_path.display())
            });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Atomic socket publication
// ---------------------------------------------------------------------------

/// Publish the leader socket atomically.
///
/// The listener binds inside a private `0700` short same-device staging
/// ancestor, so binding never depends on the length of the published path;
/// the socket is forced to `0600` while it is still unreachable inside the
/// staging directory, and only then `rename(2)`d onto the published path.
fn publish_listener(socket_path: &Path) -> Result<UnixListener> {
    let parent = socket_path.parent().ok_or_else(|| {
        anyhow!(
            "the grok shim leader socket path {} has no parent directory",
            socket_path.display()
        )
    })?;
    std::fs::create_dir_all(parent).with_context(|| {
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
    // rename(2) publishes atomically: a pager client either sees no socket or
    // sees the finished 0600 socket, never a partially published one, and the
    // bound socket keeps serving from its new path.
    if let Err(error) = std::fs::rename(&staged_socket, socket_path) {
        let _ = std::fs::remove_file(&staged_socket);
        return Err(error).with_context(|| {
            format!("publishing the grok shim socket at {}", socket_path.display())
        });
    }
    Ok(listener)
}

fn force_socket_mode(socket: &Path) -> Result<()> {
    std::fs::set_permissions(socket, Permissions::from_mode(PRIVATE_FILE_MODE))
        .with_context(|| {
            format!("forcing mode 0600 on the grok shim socket {}", socket.display())
        })?;
    let mode = std::fs::metadata(socket)
        .with_context(|| {
            format!("reading the mode of the grok shim socket {}", socket.display())
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
    /// Create a fresh private staging directory for publishing a socket whose
    /// parent is `final_parent`.
    ///
    /// Candidates are the same-device ancestors of `final_parent`, shortest
    /// first, so the bind path stays short even when the published path is
    /// near the `sun_path` limit. `rename(2)` to the published path stays on
    /// one filesystem because the staging directory shares the parent's
    /// device.
    fn create(final_parent: &Path) -> Result<Self> {
        let mut last_error: Option<std::io::Error> = None;
        for ancestor in staging_candidate_ancestors(final_parent) {
            for _ in 0..STAGING_ATTEMPTS {
                let candidate = ancestor.join(staging_dir_name());
                match std::fs::create_dir(&candidate) {
                    Ok(()) => {
                        if let Err(error) =
                            std::fs::set_permissions(&candidate, Permissions::from_mode(STAGING_DIR_MODE))
                        {
                            last_error = Some(error);
                            let _ = std::fs::remove_dir(&candidate);
                            break;
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

    /// Remove the (now empty) staging directory. Best effort: a leftover empty
    /// directory is harmless, unlike a leftover socket.
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

/// Same-device ancestors of `final_parent`, ordered shortest (shallowest)
/// first. Ancestors on a different device are skipped: publishing from them
/// could not `rename(2)` onto the final path.
fn staging_candidate_ancestors(final_parent: &Path) -> Vec<PathBuf> {
    let parent_dev = match std::fs::metadata(final_parent) {
        Ok(metadata) => metadata.dev(),
        Err(_) => return Vec::new(),
    };
    let mut candidates = Vec::new();
    for ancestor in final_parent.ancestors() {
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
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use tokio::time::{sleep, timeout, Instant};

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    /// Test double for the ACP delegate: records payloads, capabilities, and
    /// disconnects, echoes every ACP payload back, and can push one deferred
    /// payload after `handle_acp` returns.
    #[derive(Default)]
    struct RecordingDelegate {
        late_push: bool,
        payloads: Mutex<Vec<String>>,
        capabilities: Mutex<Option<(bool, bool, bool, Option<String>)>>,
        disconnects: AtomicUsize,
    }

    impl RecordingDelegate {
        fn with_late_push() -> Arc<Self> {
            Arc::new(Self {
                late_push: true,
                ..Default::default()
            })
        }

        fn disconnect_count(&self) -> usize {
            self.disconnects.load(Ordering::SeqCst)
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
                outbound.send(payload.to_string())?;
                if self.late_push {
                    let outbound = outbound.clone();
                    tokio::spawn(async move {
                        sleep(Duration::from_millis(10)).await;
                        let _ = outbound.send(
                            json!({
                                "jsonrpc": "2.0",
                                "method": "deferred",
                                "params": {},
                            })
                            .to_string(),
                        );
                    });
                }
                Ok(())
            })
        }

        fn on_client_capabilities<'a>(
            &'a self,
            capabilities: &'a ClientCapabilities,
        ) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                *self.capabilities.lock().expect("capability log") = Some((
                    capabilities.yolo_mode,
                    capabilities.auto_mode,
                    capabilities.terminal,
                    capabilities.client_version.clone(),
                ));
            })
        }

        fn on_disconnect(&self) -> BoxFuture<'_, ()> {
            Box::pin(async move {
                self.disconnects.fetch_add(1, Ordering::SeqCst);
            })
        }
    }

    // -- fixtures and helpers ------------------------------------------------

    /// An explicit short root for near-limit path tests, per the slice
    /// contract. Falls back to the platform temp dir when `/tmp` is absent.
    fn short_test_root() -> PathBuf {
        let explicit = Path::new("/tmp");
        if explicit.is_dir() {
            explicit.to_path_buf()
        } else {
            std::env::temp_dir()
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

    /// A socket path whose *filename* is near the `sun_path` limit.
    fn long_filename_socket_path(root: &Path, target_bytes: usize) -> PathBuf {
        let file_name_len = target_bytes - root.as_os_str().len() - 1;
        let stem_len = file_name_len - ".sock".len();
        assert!(
            stem_len > 8,
            "the test root is too long to exercise a near-limit filename"
        );
        root.join(format!("{}.sock", "f".repeat(stem_len)))
    }

    /// A socket path whose *parent chain* is near the `sun_path` limit.
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
            mode: "stdio".to_string(),
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
        timeout(TEST_TIMEOUT, write_frame(writer, envelope))
            .await
            .expect("writing a client frame should not time out")
            .expect("writing a client frame should succeed");
    }

    async fn next_server_frame(reader: &mut OwnedReadHalf) -> Option<ServerEnvelope> {
        timeout(TEST_TIMEOUT, read_frame::<_, ServerEnvelope>(reader))
            .await
            .expect("reading a server frame should not time out")
            .expect("reading a server frame should succeed")
    }

    /// Connect, send a valid register, and assert the exact `registered`
    /// shape the audited wire requires.
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
        let metadata =
            std::fs::metadata(socket).unwrap_or_else(|error| {
                panic!("the published socket {} should exist: {error}", socket.display())
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
        assert!(
            !handle.lock_path().exists(),
            "a clean stop must remove the leader lock file"
        );
    }

    /// Drive one full pager exchange through the production leader: register,
    /// ping/pong, ACP round trip, disconnect, and disconnect observation.
    async fn exercise_leader(socket: &Path, delegate: Arc<RecordingDelegate>) {
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
        assert_eq!(
            *delegate.payloads.lock().expect("payload log"),
            vec![payload],
            "the delegate should have seen exactly the dispatched payload"
        );

        write_client_frame(&mut writer, &ClientEnvelope::Disconnect).await;
        assert!(
            next_server_frame(&mut reader).await.is_none(),
            "the leader should close the connection after disconnect"
        );
        // The server drops the connection only after on_disconnect ran, so
        // observing EOF proves the disconnect notification was delivered.
        assert!(
            delegate.disconnect_count() >= 1,
            "the delegate should observe the disconnect"
        );
    }

    /// Spawn a leader, retrying while a just-aborted previous leader is still
    /// releasing its lock (task abort is asynchronous by nature).
    async fn spawn_with_retry(socket: &Path, delegate: Arc<RecordingDelegate>) -> LeaderHandle {
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            match spawn_leader(LeaderServerConfig::new(socket.to_path_buf()), delegate.clone()) {
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

    // -- pure helpers --------------------------------------------------------

    #[test]
    fn leader_binary_version_is_the_prefixed_package_version() {
        assert_eq!(
            leader_binary_version(),
            format!("gents-{}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn register_validation_rejects_blank_and_unknown_values() {
        assert!(validate_register("grok-pager", "stdio").is_ok());
        assert!(validate_register("grok-pager", "headless").is_ok());
        assert!(validate_register("", "stdio").is_err());
        assert!(validate_register("   ", "stdio").is_err());
        assert!(validate_register("grok-pager", "widget").is_err());
    }

    #[test]
    fn internal_error_responses_target_requests_only() {
        assert!(
            internal_error_response(r#"{"jsonrpc":"2.0","method":"x"}"#).is_none(),
            "notifications must not be answered"
        );
        assert!(
            internal_error_response("not json").is_none(),
            "undecodable payloads must not be answered"
        );
        let response = internal_error_response(r#"{"jsonrpc":"2.0","id":7,"method":"x"}"#)
            .expect("requests must be answered");
        let value: Value =
            serde_json::from_str(&response).expect("the failure response is valid JSON-RPC");
        assert_eq!(value["id"], json!(7));
        assert_eq!(value["error"]["code"], json!(JSONRPC_INTERNAL_ERROR));
    }

    #[test]
    fn staging_candidates_prefer_the_shortest_same_device_ancestor() {
        let root = unique_test_root("staging-order");
        let parent = root.join("a").join("b");
        std::fs::create_dir_all(&parent).expect("creating nested test dirs");
        let candidates = staging_candidate_ancestors(&parent);
        assert!(!candidates.is_empty(), "at least the parent itself qualifies");
        let parent_dev = std::fs::metadata(&parent).expect("parent metadata").dev();
        for candidate in &candidates {
            assert_eq!(
                std::fs::metadata(candidate)
                    .expect("candidate metadata")
                    .dev(),
                parent_dev,
                "every candidate must share the socket parent's device"
            );
            assert!(
                parent.starts_with(candidate),
                "every candidate must be an ancestor of (or equal to) the socket parent"
            );
        }
        let lengths: Vec<usize> = candidates.iter().map(|path| path.as_os_str().len()).collect();
        let mut sorted = lengths.clone();
        sorted.sort_unstable();
        assert_eq!(lengths, sorted, "candidates must be ordered shortest first");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_staging_dir_is_private_and_removed_on_cleanup() {
        let root = unique_test_root("staging-dir");
        let staging = StagingDir::create(&root).expect("creating the staging ancestor");
        let mode = std::fs::metadata(&staging.path)
            .expect("staging metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "the staging ancestor must be private");
        staging.cleanup();
        assert!(
            !staging.path.exists(),
            "cleanup must remove the staging ancestor"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // -- publication ---------------------------------------------------------

    #[tokio::test]
    async fn over_limit_paths_still_bind_and_publish() {
        let root = unique_test_root("overlimit");
        let socket = long_parent_socket_path(&root, 200);
        assert!(
            socket.as_os_str().len() > 110,
            "the published path must exceed every sun_path limit (got {})",
            socket.as_os_str().len()
        );
        // Binding happens inside the short staging ancestor, so publication
        // never depends on the length of the published path.
        let listener = publish_listener(&socket)
            .expect("binding must not depend on the published path length");
        let metadata = std::fs::metadata(&socket)
            .expect("the socket must be published at the long path");
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        drop(listener);
        std::fs::remove_file(&socket).expect("cleaning up the published socket");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn near_limit_long_filename_binds_and_connects() {
        let root = unique_test_root("longfile");
        let socket = long_filename_socket_path(&root, NEAR_LIMIT_PATH_BYTES);
        assert!(socket.as_os_str().len() <= NEAR_LIMIT_PATH_BYTES);
        let delegate = Arc::new(RecordingDelegate::default());
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), delegate.clone())
            .expect("the leader should spawn near the path-length limit");
        assert_socket_mode_0600(&socket);
        exercise_leader(&socket, delegate).await;
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn near_limit_long_parent_binds_and_connects() {
        let root = unique_test_root("longparent");
        let socket = long_parent_socket_path(&root, NEAR_LIMIT_PATH_BYTES);
        // The parent directories do not exist yet: publication must create
        // them, stage the bind in a short ancestor, and rename into place.
        let delegate = Arc::new(RecordingDelegate::default());
        let mut handle = spawn_leader(LeaderServerConfig::new(socket.clone()), delegate.clone())
            .expect("the leader should spawn near the path-length limit");
        assert_socket_mode_0600(&socket);
        exercise_leader(&socket, delegate).await;
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    // -- spawn lifetime, election, and cleanup -------------------------------

    #[tokio::test]
    async fn the_production_spawn_lifetime_publishes_serves_and_cleans_up() {
        let root = unique_test_root("lifetime");
        let socket = root.join("leader.sock");
        let lock_path = socket.with_extension("lock");
        let delegate = Arc::new(RecordingDelegate::default());
        let mut handle =
            spawn_leader(LeaderServerConfig::new(socket.clone()), delegate.clone())
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
        exercise_leader(&socket, delegate).await;
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_second_leader_fails_while_the_first_holds_the_lock() {
        let root = unique_test_root("exclusive");
        let socket = root.join("leader.sock");
        let lock_path = socket.with_extension("lock");
        let first_delegate = Arc::new(RecordingDelegate::default());
        let mut first =
            spawn_leader(LeaderServerConfig::new(socket.clone()), first_delegate.clone())
                .expect("the first leader should spawn");

        let second = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        );
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

        // The winner still serves traffic through the production path.
        exercise_leader(&socket, first_delegate).await;

        first.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&first);

        // The lock is free again, so a new leader can take the socket.
        let mut second =
            spawn_leader(LeaderServerConfig::new(socket.clone()), Arc::new(RecordingDelegate::default()))
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
        let mut handle = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        )
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
        let spawn = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        );
        assert!(spawn.is_err(), "a non-socket path must be refused");
        assert_eq!(
            std::fs::read_to_string(&socket).expect("the blocking file should be untouched"),
            "not a socket\n"
        );
        assert!(
            !socket.with_extension("lock").exists(),
            "a failed spawn must release and remove its lock file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn dropping_the_handle_unlinks_the_socket_and_leaves_a_reclaimable_lock() {
        let root = unique_test_root("drop");
        let socket = root.join("leader.sock");
        let handle = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        )
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
        let delegate = Arc::new(RecordingDelegate::default());
        let mut next = spawn_with_retry(&socket, delegate.clone()).await;
        exercise_leader(&socket, delegate).await;
        next.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&next);
        let _ = std::fs::remove_dir_all(&root);
    }

    // -- registration order and protocol handling ----------------------------

    #[tokio::test]
    async fn register_must_precede_registered_and_validate() {
        let root = unique_test_root("register");
        let socket = root.join("leader.sock");
        let mut handle = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        )
        .expect("the leader should spawn");

        // ping before register is a protocol violation.
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
        // acp before register is a protocol violation.
        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(
                &mut writer,
                &ClientEnvelope::Acp {
                    payload: json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})
                        .to_string(),
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
        // an unknown register mode is rejected.
        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(
                &mut writer,
                &ClientEnvelope::Register {
                    client_type: "grok-pager".to_string(),
                    mode: "widget".to_string(),
                    capabilities: test_capabilities(),
                },
            )
            .await;
            match next_server_frame(&mut reader).await {
                Some(ServerEnvelope::Error { code, message }) => {
                    assert_eq!(code, ENVELOPE_ERROR_INVALID_REQUEST);
                    assert!(message.contains("widget"), "the error should name the bad mode");
                }
                other => panic!("expected an error envelope, got {other:?}"),
            }
            assert!(next_server_frame(&mut reader).await.is_none());
        }
        // a blank client_type is rejected.
        {
            let (mut reader, mut writer) = connect(&socket).await;
            write_client_frame(
                &mut writer,
                &ClientEnvelope::Register {
                    client_type: "   ".to_string(),
                    mode: "stdio".to_string(),
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
        // a valid register still works afterwards.
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
        let mut handle = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        )
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
        let delegate = Arc::new(RecordingDelegate::default());
        let mut handle =
            spawn_leader(LeaderServerConfig::new(socket.clone()), delegate.clone())
                .expect("the leader should spawn");
        let (mut reader, mut writer, _client_id) = register(&socket).await;
        // The server runs on_client_capabilities before it reads any further
        // frame, so a completed ping/pong proves the capture happened.
        write_client_frame(&mut writer, &ClientEnvelope::Ping).await;
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::Pong) => {}
            other => panic!("expected pong, got {other:?}"),
        }
        assert_eq!(
            delegate
                .capabilities
                .lock()
                .expect("capability log")
                .clone(),
            Some((true, false, false, Some("grok-pager-test".to_string()))),
            "yolo_mode/auto_mode/terminal and the client version must be captured"
        );
        handle.shutdown().await.expect("clean shutdown");
        assert_clean_stop(&handle);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn control_commands_answer_method_not_found() {
        let root = unique_test_root("control");
        let socket = root.join("leader.sock");
        let mut handle = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        )
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
        let delegate = RecordingDelegate::with_late_push();
        let mut handle =
            spawn_leader(LeaderServerConfig::new(socket.clone()), delegate.clone())
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
        // The deferred push arrives after handle_acp returned, on the same
        // connection, through the cloned outbound handle.
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
    async fn shutdown_announces_the_stop_to_live_connections() {
        let root = unique_test_root("shutdown-frames");
        let socket = root.join("leader.sock");
        let mut handle = spawn_leader(
            LeaderServerConfig::new(socket.clone()),
            Arc::new(RecordingDelegate::default()),
        )
        .expect("the leader should spawn");
        let (mut reader, _writer, _client_id) = register(&socket).await;
        handle.shutdown().await.expect("clean shutdown");
        match next_server_frame(&mut reader).await {
            Some(ServerEnvelope::ShuttingDown { reason, delay_ms }) => {
                assert_eq!(reason, "manual");
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
