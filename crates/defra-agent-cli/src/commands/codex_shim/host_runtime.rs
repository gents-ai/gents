use std::cmp::Reverse;
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use codex_app_server_protocol as codex;
use tokio::process::Command;

use super::protocol::send_notification;
use super::{ConnectionState, ShimState, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS};

const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FUZZY_RESULTS: usize = 100;
const MAX_FUZZY_WALK_ENTRIES: usize = 25_000;

#[derive(Debug)]
pub(super) struct HostRuntimeError {
    pub(super) code: i64,
    pub(super) message: String,
}

pub(super) async fn git_diff_to_remote(
    state: &ShimState,
    params: codex::GitDiffToRemoteParams,
) -> Result<codex::GitDiffToRemoteResponse, HostRuntimeError> {
    let cwd = resolve_runtime_cwd(state, Some(params.cwd.as_path()))?;
    let _repo_root = run_git(&cwd, &["rev-parse", "--show-toplevel"]).await?;
    let base_sha = match run_git(&cwd, &["merge-base", "HEAD", "@{upstream}"]).await {
        Ok(sha) => sha,
        Err(_) => run_git(&cwd, &["rev-parse", "HEAD"]).await?,
    };
    let base_sha = base_sha.trim().to_string();
    if base_sha.is_empty() {
        return Err(HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("failed to compute git base sha for {}", cwd.display()),
        });
    }

    let mut diff = run_git_allowing_diff_exit(
        &cwd,
        &["diff", "--no-textconv", "--no-ext-diff", base_sha.as_str()],
    )
    .await?;
    diff.push_str(&untracked_git_diff(&cwd).await?);

    Ok(codex::GitDiffToRemoteResponse {
        sha: codex::GitSha::new(&base_sha),
        diff,
    })
}

pub(super) async fn fuzzy_file_search(
    state: &ShimState,
    params: codex::FuzzyFileSearchParams,
) -> Result<codex::FuzzyFileSearchResponse, HostRuntimeError> {
    let files = search_roots(state, &params.query, params.roots).await?;
    Ok(codex::FuzzyFileSearchResponse { files })
}

pub(super) async fn fuzzy_file_search_session_start(
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

pub(super) async fn fuzzy_file_search_session_update(
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

pub(super) async fn fuzzy_file_search_session_stop(
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

async fn run_git(cwd: &Path, args: &[&str]) -> Result<String, HostRuntimeError> {
    let output = run_git_output(cwd, args).await?;
    if !output.status.success() {
        return Err(HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_git_allowing_diff_exit(cwd: &Path, args: &[&str]) -> Result<String, HostRuntimeError> {
    let output = run_git_output(cwd, args).await?;
    let exit_ok = output
        .status
        .code()
        .is_some_and(|code| code == 0 || code == 1);
    if !exit_ok {
        return Err(HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

async fn run_git_output(
    cwd: &Path,
    args: &[&str],
) -> Result<std::process::Output, HostRuntimeError> {
    let mut command = Command::new("git");
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    tokio::time::timeout(GIT_COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("git {} timed out", args.join(" ")),
        })?
        .map_err(|err| HostRuntimeError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("failed to run git {}: {err}", args.join(" ")),
        })
}

async fn untracked_git_diff(cwd: &Path) -> Result<String, HostRuntimeError> {
    let output = run_git_output(cwd, &["ls-files", "--others", "--exclude-standard"]).await?;
    if !output.status.success() {
        return Ok(String::new());
    }
    let untracked = String::from_utf8_lossy(&output.stdout);
    let mut diff = String::new();
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    for file in untracked.lines().filter(|line| !line.is_empty()) {
        let output = run_git_output(
            cwd,
            &[
                "diff",
                "--no-textconv",
                "--no-ext-diff",
                "--binary",
                "--no-index",
                "--",
                null_device,
                file,
            ],
        )
        .await?;
        if output
            .status
            .code()
            .is_some_and(|code| code == 0 || code == 1)
        {
            diff.push_str(&String::from_utf8_lossy(&output.stdout));
        }
    }
    Ok(diff)
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

fn resolve_runtime_cwd(
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

fn resolve_runtime_path(state: &ShimState, path: &Path) -> Result<PathBuf, HostRuntimeError> {
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

fn classify_io_error(error: io::Error) -> HostRuntimeError {
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
