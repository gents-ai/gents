use std::io;
use std::path::{Component, Path, PathBuf};

use crate::commands::codex_shim::{ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS};

use super::HostRuntimeError;

pub(super) fn resolve_runtime_cwd(
    state: &ShimState,
    path: Option<&Path>,
) -> Result<PathBuf, HostRuntimeError> {
    let path = match path {
        Some(path) => resolve_runtime_path(state, path)?,
        None => state
            .fs_root
            .as_ref()
            .map_or_else(|| state.cwd.clone(), PathBuf::from),
    };
    let path = canonicalize_path(&path)?;
    if !path.is_dir() {
        return Err(HostRuntimeError {
            code: JSONRPC_INVALID_PARAMS,
            message: format!("working directory is not a directory: {}", path.display()),
        });
    }
    ensure_under_tool_root(state, path)
}

pub(super) fn resolve_runtime_path(
    state: &ShimState,
    path: &Path,
) -> Result<PathBuf, HostRuntimeError> {
    let base = state
        .fs_root
        .as_ref()
        .map_or_else(|| state.cwd.clone(), PathBuf::from);
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let candidate = normalize_path(&candidate)?;
    let resolved = canonicalize_path(&candidate)?;
    ensure_under_tool_root(state, resolved)
}

fn ensure_under_tool_root(state: &ShimState, path: PathBuf) -> Result<PathBuf, HostRuntimeError> {
    if let Some(root) = &state.fs_root {
        let root = canonicalize_path(root)?;
        if !path.starts_with(&root) {
            return Err(HostRuntimeError {
                code: JSONRPC_INVALID_PARAMS,
                message: format!(
                    "path is outside the allowed tool root {}: {}",
                    root.display(),
                    path.display()
                ),
            });
        }
    }
    Ok(path)
}

fn canonicalize_path(path: &Path) -> Result<PathBuf, HostRuntimeError> {
    std::fs::canonicalize(path).map_err(|err| HostRuntimeError {
        code: if err.kind() == io::ErrorKind::NotFound {
            JSONRPC_INVALID_PARAMS
        } else {
            JSONRPC_INTERNAL_ERROR
        },
        message: format!("resolving path {}: {err}", path.display()),
    })
}

fn normalize_path(path: &Path) -> Result<PathBuf, HostRuntimeError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        Err(HostRuntimeError {
            code: JSONRPC_INVALID_PARAMS,
            message: format!(
                "path did not resolve to an absolute path: {}",
                path.display()
            ),
        })
    }
}

pub(super) fn classify_io_error(error: io::Error) -> HostRuntimeError {
    let code = if error.kind() == io::ErrorKind::InvalidInput {
        JSONRPC_INVALID_PARAMS
    } else {
        JSONRPC_INTERNAL_ERROR
    };
    HostRuntimeError {
        code,
        message: error.to_string(),
    }
}
