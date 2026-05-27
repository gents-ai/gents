use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use codex_app_server_protocol as codex;
use codex_utils_absolute_path::AbsolutePathBuf;
use defra_native_fs_runner::host_fs::{CopyOptions, CreateDirectoryOptions, HostFs, RemoveOptions};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::protocol::send_notification;
use super::{ConnectionState, Outbound, ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS};

const FS_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(200);

// Codex fs/* routes are byte-oriented host runtime requests. The filesystem
// policy and operations live in defra-native-fs-runner; this module only maps
// Codex's base64/JSON-RPC envelope onto those DEFRA primitives.
#[derive(Debug)]
pub(super) struct FsAdapterError {
    pub(super) code: i64,
    pub(super) message: String,
}

pub(super) struct FsWatchRegistration {
    cancel_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
}

pub(super) async fn read_file(
    state: &ShimState,
    params: codex::FsReadFileParams,
) -> std::result::Result<codex::FsReadFileResponse, FsAdapterError> {
    let bytes = run_host_fs(state, move |fs| fs.read_file(params.path.as_path())).await?;
    Ok(codex::FsReadFileResponse {
        data_base64: STANDARD.encode(bytes),
    })
}

pub(super) async fn write_file(
    state: &ShimState,
    params: codex::FsWriteFileParams,
) -> std::result::Result<codex::FsWriteFileResponse, FsAdapterError> {
    ensure_writes_enabled(state)?;
    let bytes = STANDARD
        .decode(params.data_base64)
        .map_err(|err| FsAdapterError {
            code: JSONRPC_INVALID_PARAMS,
            message: format!("fs/writeFile requires valid base64 dataBase64: {err}"),
        })?;
    run_host_fs(state, move |fs| {
        fs.write_file(params.path.as_path(), &bytes)
    })
    .await?;
    Ok(codex::FsWriteFileResponse {})
}

pub(super) async fn create_directory(
    state: &ShimState,
    params: codex::FsCreateDirectoryParams,
) -> std::result::Result<codex::FsCreateDirectoryResponse, FsAdapterError> {
    ensure_writes_enabled(state)?;
    run_host_fs(state, move |fs| {
        fs.create_directory(
            params.path.as_path(),
            CreateDirectoryOptions {
                recursive: params.recursive.unwrap_or(true),
            },
        )
    })
    .await?;
    Ok(codex::FsCreateDirectoryResponse {})
}

pub(super) async fn get_metadata(
    state: &ShimState,
    params: codex::FsGetMetadataParams,
) -> std::result::Result<codex::FsGetMetadataResponse, FsAdapterError> {
    let metadata = run_host_fs(state, move |fs| fs.get_metadata(params.path.as_path())).await?;
    Ok(codex::FsGetMetadataResponse {
        is_directory: metadata.is_directory,
        is_file: metadata.is_file,
        is_symlink: metadata.is_symlink,
        created_at_ms: metadata.created_at_ms,
        modified_at_ms: metadata.modified_at_ms,
    })
}

pub(super) async fn read_directory(
    state: &ShimState,
    params: codex::FsReadDirectoryParams,
) -> std::result::Result<codex::FsReadDirectoryResponse, FsAdapterError> {
    let entries = run_host_fs(state, move |fs| fs.read_directory(params.path.as_path())).await?;
    Ok(codex::FsReadDirectoryResponse {
        entries: entries
            .into_iter()
            .map(|entry| codex::FsReadDirectoryEntry {
                file_name: entry.file_name,
                is_directory: entry.is_directory,
                is_file: entry.is_file,
            })
            .collect(),
    })
}

pub(super) async fn remove(
    state: &ShimState,
    params: codex::FsRemoveParams,
) -> std::result::Result<codex::FsRemoveResponse, FsAdapterError> {
    ensure_writes_enabled(state)?;
    run_host_fs(state, move |fs| {
        fs.remove(
            params.path.as_path(),
            RemoveOptions {
                recursive: params.recursive.unwrap_or(true),
                force: params.force.unwrap_or(true),
            },
        )
    })
    .await?;
    Ok(codex::FsRemoveResponse {})
}

pub(super) async fn copy(
    state: &ShimState,
    params: codex::FsCopyParams,
) -> std::result::Result<codex::FsCopyResponse, FsAdapterError> {
    ensure_writes_enabled(state)?;
    run_host_fs(state, move |fs| {
        fs.copy(
            params.source_path.as_path(),
            params.destination_path.as_path(),
            CopyOptions {
                recursive: params.recursive,
            },
        )
    })
    .await?;
    Ok(codex::FsCopyResponse {})
}

pub(super) async fn watch(
    connection: &ConnectionState,
    state: &ShimState,
    params: codex::FsWatchParams,
) -> std::result::Result<codex::FsWatchResponse, FsAdapterError> {
    run_host_fs(state, {
        let path = params.path.to_path_buf();
        move |fs| fs.validate_watch_path(path)
    })
    .await?;

    let watch_id = params.watch_id.clone();
    let mut watches = connection.fs_watches.lock().await;
    if watches.contains_key(&watch_id) {
        return Err(FsAdapterError {
            code: JSONRPC_INVALID_PARAMS,
            message: format!("watchId already exists: {watch_id}"),
        });
    }

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let task = spawn_watch_task(
        state.clone(),
        connection.outbound.clone(),
        watch_id.clone(),
        params.path.clone(),
        cancel_rx,
    );
    watches.insert(watch_id, FsWatchRegistration { cancel_tx, task });
    Ok(codex::FsWatchResponse { path: params.path })
}

pub(super) async fn unwatch(
    connection: &ConnectionState,
    params: codex::FsUnwatchParams,
) -> std::result::Result<codex::FsUnwatchResponse, FsAdapterError> {
    if let Some(registration) = connection.fs_watches.lock().await.remove(&params.watch_id) {
        stop_watch(registration).await;
    }
    Ok(codex::FsUnwatchResponse {})
}

pub(super) async fn close_all_watches(connection: &ConnectionState) {
    let watches = std::mem::take(&mut *connection.fs_watches.lock().await);
    for registration in watches.into_values() {
        stop_watch(registration).await;
    }
}

async fn run_host_fs<T, F>(state: &ShimState, f: F) -> std::result::Result<T, FsAdapterError>
where
    T: Send + 'static,
    F: FnOnce(HostFs) -> io::Result<T> + Send + 'static,
{
    let Some(root) = state.fs_root.clone() else {
        return Err(FsAdapterError {
            code: JSONRPC_INTERNAL_ERROR,
            message: "local filesystem is not configured".to_string(),
        });
    };
    tokio::task::spawn_blocking(move || {
        let fs = HostFs::new_with_base(root.clone(), Some(root)).map_err(classify_io_error)?;
        f(fs).map_err(classify_io_error)
    })
    .await
    .map_err(|err| FsAdapterError {
        code: JSONRPC_INTERNAL_ERROR,
        message: format!("host filesystem task failed: {err}"),
    })?
}

fn ensure_writes_enabled(state: &ShimState) -> std::result::Result<(), FsAdapterError> {
    if state.fs_writes_enabled {
        Ok(())
    } else {
        Err(FsAdapterError {
            code: JSONRPC_INVALID_PARAMS,
            message: "filesystem writes are disabled by the DEFRA tool ceiling".to_string(),
        })
    }
}

fn classify_io_error(error: io::Error) -> FsAdapterError {
    let code = if error.kind() == io::ErrorKind::InvalidInput {
        JSONRPC_INVALID_PARAMS
    } else {
        JSONRPC_INTERNAL_ERROR
    };
    FsAdapterError {
        code,
        message: error.to_string(),
    }
}

fn spawn_watch_task(
    state: ShimState,
    outbound: Outbound,
    watch_id: String,
    watch_path: AbsolutePathBuf,
    mut cancel_rx: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let watch_root = watch_path.to_path_buf();
        let mut previous = capture_watch_snapshot(watch_root.as_path()).unwrap_or_default();
        let mut interval = tokio::time::interval(FS_WATCH_POLL_INTERVAL);
        loop {
            tokio::select! {
                _ = cancel_rx.changed() => break,
                _ = interval.tick() => {
                    let current = capture_watch_snapshot(watch_root.as_path()).unwrap_or_default();
                    let changed = changed_paths(&previous, &current);
                    previous = current;
                    if changed.is_empty() {
                        continue;
                    }
                    let changed_paths = changed
                        .into_iter()
                        .filter_map(|path| codex_watch_path(&watch_path, &path))
                        .collect::<Vec<_>>();
                    if changed_paths.is_empty() {
                        continue;
                    }
                    let _ = send_notification(
                        &outbound,
                        &state,
                        codex::ServerNotification::FsChanged(codex::FsChangedNotification {
                            watch_id: watch_id.clone(),
                            changed_paths,
                        }),
                    )
                    .await;
                }
            }
        }
    })
}

async fn stop_watch(registration: FsWatchRegistration) {
    let _ = registration.cancel_tx.send(true);
    let _ = registration.task.await;
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WatchFingerprint {
    is_dir: bool,
    is_file: bool,
    is_symlink: bool,
    len: u64,
    modified_ms: i64,
}

fn capture_watch_snapshot(path: &Path) -> io::Result<BTreeMap<PathBuf, WatchFingerprint>> {
    let mut snapshot = BTreeMap::new();
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                let child = entry.path();
                if let Ok(fingerprint) = watch_fingerprint(child.as_path()) {
                    snapshot.insert(child, fingerprint);
                }
            }
        }
        Ok(_) => {
            if let Ok(fingerprint) = watch_fingerprint(path) {
                snapshot.insert(path.to_path_buf(), fingerprint);
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    Ok(snapshot)
}

fn watch_fingerprint(path: &Path) -> io::Result<WatchFingerprint> {
    let metadata = std::fs::metadata(path)?;
    let symlink_metadata = std::fs::symlink_metadata(path)?;
    Ok(WatchFingerprint {
        is_dir: metadata.is_dir(),
        is_file: metadata.is_file(),
        is_symlink: symlink_metadata.file_type().is_symlink(),
        len: metadata.len(),
        modified_ms: metadata.modified().ok().map_or(0, system_time_to_unix_ms),
    })
}

fn changed_paths(
    previous: &BTreeMap<PathBuf, WatchFingerprint>,
    current: &BTreeMap<PathBuf, WatchFingerprint>,
) -> Vec<PathBuf> {
    let keys = previous
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    keys.into_iter()
        .filter(|path| previous.get(path) != current.get(path))
        .collect()
}

fn codex_watch_path(watch_path: &AbsolutePathBuf, changed_path: &Path) -> Option<AbsolutePathBuf> {
    let root = watch_path.as_path();
    if changed_path == root {
        return Some(watch_path.clone());
    }
    changed_path
        .strip_prefix(root)
        .ok()
        .map(|relative| watch_path.join(relative))
}

fn system_time_to_unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
