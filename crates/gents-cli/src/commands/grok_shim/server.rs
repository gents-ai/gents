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

mod connection;
mod lock;
mod socket;
#[cfg(test)]
mod tests;

use connection::accept_loop;
use lock::LeaderLock;
use socket::{publish_listener, remove_stale_socket};

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

/// A disconnected peer cannot hold connection cleanup open indefinitely while
/// dispatch tasks drain or the writer flushes queued frames.
const MAX_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Internal bound for joining live connection tasks during leader shutdown,
/// independent of the pager-facing shutdown envelope whose delay remains zero.
const CONNECTION_CLEANUP_GRACE: Duration = Duration::from_secs(5);

const WIRE_SHUTDOWN_DELAY_MS: u64 = 0;

/// Backpressure bounds frames retained for a peer that is slow to read.
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
