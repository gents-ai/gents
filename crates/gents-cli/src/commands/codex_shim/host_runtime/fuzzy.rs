use std::cmp::Reverse;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use gents_codex_protocol as codex;

use crate::commands::codex_shim::protocol::send_notification;
use crate::commands::codex_shim::{
    ConnectionState, ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS,
};

use super::paths::{classify_io_error, resolve_runtime_path};
use super::HostRuntimeError;

const MAX_FUZZY_RESULTS: usize = 100;
const MAX_FUZZY_WALK_ENTRIES: usize = 25_000;

pub(in crate::commands::codex_shim) async fn fuzzy_file_search(
    state: &ShimState,
    params: codex::FuzzyFileSearchParams,
) -> Result<codex::FuzzyFileSearchResponse, HostRuntimeError> {
    let files = search_roots(state, &params.query, params.roots).await?;
    Ok(codex::FuzzyFileSearchResponse { files })
}

pub(in crate::commands::codex_shim) async fn fuzzy_file_search_session_start(
    connection: &ConnectionState,
    params: codex::FuzzyFileSearchSessionStartParams,
) -> Result<codex::FuzzyFileSearchSessionStartResponse, HostRuntimeError> {
    if params.session_id.is_empty() {
        return Err(HostRuntimeError {
            code: JSONRPC_INVALID_PARAMS,
            message: "fuzzyFileSearch/sessionStart requires a non-empty sessionId".to_string(),
        });
    }
    connection
        .fuzzy_file_search_sessions
        .lock()
        .await
        .insert(params.session_id, params.roots);
    Ok(codex::FuzzyFileSearchSessionStartResponse {})
}

pub(in crate::commands::codex_shim) async fn fuzzy_file_search_session_update(
    connection: &ConnectionState,
    state: &ShimState,
    params: codex::FuzzyFileSearchSessionUpdateParams,
) -> Result<codex::FuzzyFileSearchSessionUpdateResponse, HostRuntimeError> {
    let Some(roots) = connection
        .fuzzy_file_search_sessions
        .lock()
        .await
        .get(&params.session_id)
        .cloned()
    else {
        return Err(HostRuntimeError {
            code: JSONRPC_INVALID_PARAMS,
            message: format!("fuzzy file search session not found: {}", params.session_id),
        });
    };

    let connection = connection.clone();
    let state = state.clone();
    tokio::spawn(async move {
        let files = match search_roots(&state, &params.query, roots).await {
            Ok(files) => files,
            Err(err) => {
                tracing::warn!(
                    code = err.code,
                    message = %err.message,
                    "Codex shim fuzzy file search session update failed"
                );
                Vec::new()
            }
        };
        if !connection
            .fuzzy_file_search_sessions
            .lock()
            .await
            .contains_key(&params.session_id)
        {
            return;
        }
        let session_id = params.session_id;
        let _ = send_notification(
            &connection.outbound,
            &state,
            codex::ServerNotification::FuzzyFileSearchSessionUpdated(
                codex::FuzzyFileSearchSessionUpdatedNotification {
                    session_id: session_id.clone(),
                    query: params.query,
                    files,
                },
            ),
        )
        .await;
        if connection
            .fuzzy_file_search_sessions
            .lock()
            .await
            .contains_key(&session_id)
        {
            let _ = send_notification(
                &connection.outbound,
                &state,
                codex::ServerNotification::FuzzyFileSearchSessionCompleted(
                    codex::FuzzyFileSearchSessionCompletedNotification { session_id },
                ),
            )
            .await;
        }
    });

    Ok(codex::FuzzyFileSearchSessionUpdateResponse {})
}

pub(in crate::commands::codex_shim) async fn fuzzy_file_search_session_stop(
    connection: &ConnectionState,
    _state: &ShimState,
    params: codex::FuzzyFileSearchSessionStopParams,
) -> Result<codex::FuzzyFileSearchSessionStopResponse, HostRuntimeError> {
    connection
        .fuzzy_file_search_sessions
        .lock()
        .await
        .remove(&params.session_id);
    Ok(codex::FuzzyFileSearchSessionStopResponse {})
}

async fn search_roots(
    state: &ShimState,
    query: &str,
    roots: Vec<String>,
) -> Result<Vec<codex::FuzzyFileSearchResult>, HostRuntimeError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let roots = resolve_search_roots(state, roots)?;
    let query = query.to_string();
    tokio::task::spawn_blocking(move || fuzzy_search_blocking(&query, roots))
        .await
        .map_err(|err| HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("fuzzy file search task failed: {err}"),
        })?
}

fn fuzzy_search_blocking(
    query: &str,
    roots: Vec<SearchRoot>,
) -> Result<Vec<codex::FuzzyFileSearchResult>, HostRuntimeError> {
    let query_lower = query.to_lowercase();
    let mut results = Vec::new();
    let mut scanned = 0usize;
    for root in roots {
        walk_search_root(&root, &query_lower, &mut scanned, &mut results)?;
        if scanned >= MAX_FUZZY_WALK_ENTRIES {
            break;
        }
    }
    results.sort_by_key(|result| {
        (
            Reverse(result.score),
            result.path.len(),
            result.path.clone(),
        )
    });
    results.truncate(MAX_FUZZY_RESULTS);
    Ok(results)
}

fn walk_search_root(
    root: &SearchRoot,
    query_lower: &str,
    scanned: &mut usize,
    results: &mut Vec<codex::FuzzyFileSearchResult>,
) -> Result<(), HostRuntimeError> {
    let mut stack = vec![root.path.clone()];
    while let Some(path) = stack.pop() {
        if *scanned >= MAX_FUZZY_WALK_ENTRIES {
            break;
        }
        *scanned += 1;
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        let is_dir = metadata.is_dir();
        if is_dir && should_skip_dir(path.file_name()) && path != root.path {
            continue;
        }
        if let Some(result) = fuzzy_result(root, &path, is_dir, query_lower) {
            results.push(result);
        }
        if is_dir {
            let entries = std::fs::read_dir(&path).map_err(classify_io_error)?;
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

fn fuzzy_result(
    root: &SearchRoot,
    path: &Path,
    is_dir: bool,
    query_lower: &str,
) -> Option<codex::FuzzyFileSearchResult> {
    if path == root.path {
        return None;
    }
    let relative = path.strip_prefix(&root.path).ok()?;
    let path_text = relative.to_string_lossy().into_owned();
    let file_name = relative
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_text.clone());
    let searchable = path_text.to_lowercase();
    let (score, indices) = fuzzy_score(&searchable, query_lower)?;
    Some(codex::FuzzyFileSearchResult {
        root: root.label.clone(),
        path: path_text,
        match_type: if is_dir {
            codex::FuzzyFileSearchMatchType::Directory
        } else {
            codex::FuzzyFileSearchMatchType::File
        },
        file_name,
        score,
        indices: Some(indices),
    })
}

fn fuzzy_score(searchable: &str, query: &str) -> Option<(u32, Vec<u32>)> {
    if let Some(start) = searchable.find(query) {
        let indices = (start..start + query.len()).map(|idx| idx as u32).collect();
        let score = 10_000u32.saturating_sub(start as u32);
        return Some((score, indices));
    }

    let mut indices = Vec::new();
    let mut query_chars = query.chars();
    let mut current = query_chars.next()?;
    for (idx, ch) in searchable.chars().enumerate() {
        if ch == current {
            indices.push(idx as u32);
            match query_chars.next() {
                Some(next) => current = next,
                None => {
                    let spread = indices
                        .last()
                        .zip(indices.first())
                        .map(|(last, first)| last.saturating_sub(*first))
                        .unwrap_or(0);
                    return Some((5_000u32.saturating_sub(spread), indices));
                }
            }
        }
    }
    None
}

fn should_skip_dir(name: Option<&OsStr>) -> bool {
    matches!(
        name.and_then(OsStr::to_str),
        Some(".git" | "target" | "node_modules" | ".lake" | ".direnv" | ".next")
    )
}

#[derive(Debug)]
struct SearchRoot {
    label: String,
    path: PathBuf,
}

fn resolve_search_roots(
    state: &ShimState,
    roots: Vec<String>,
) -> Result<Vec<SearchRoot>, HostRuntimeError> {
    let roots = if roots.is_empty() {
        vec![".".to_string()]
    } else {
        roots
    };
    roots
        .into_iter()
        .map(|label| {
            let path = resolve_runtime_path(state, Path::new(&label))?;
            Ok(SearchRoot { label, path })
        })
        .collect()
}
