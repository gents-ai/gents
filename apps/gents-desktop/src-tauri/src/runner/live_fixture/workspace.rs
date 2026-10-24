use std::path::{Path, PathBuf};

type Result<T, E = WorkspaceError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub(super) enum WorkspaceError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("creating seeded workspace {}", path.display())]
    CreateWorkspace {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("creating {}", path.display())]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading {}", path.display())]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("copying {} -> {}", source_path.display(), target_path.display())]
    Copy {
        source_path: PathBuf,
        target_path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to locate repo root from {}", manifest_dir.display())]
    RepoRoot { manifest_dir: PathBuf },
}

pub(super) fn seed_runner_node_home(
    node_home: &Path,
    node_name: &str,
    node_did: &str,
    operator_graphql: &str,
    peer_id: &str,
    listen_address: &str,
) -> Result<()> {
    std::fs::create_dir_all(node_home)?;
    std::fs::write(
        node_home.join("init.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "node_name": node_name,
            "node_did": node_did,
        }))?,
    )?;
    std::fs::write(
        node_home.join("runtime.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "graphql": operator_graphql,
            "node_name": node_name,
            "node_did": node_did,
            "p2p_transport": "iroh",
            "p2p_peer_id": peer_id,
            "p2p_listen_addresses": [listen_address],
        }))?,
    )?;
    Ok(())
}

pub(super) fn seed_repo_workspace(tool_root: &Path) -> Result<()> {
    let repo_root = workspace_repo_root()?;
    let workspace_root = tool_root.join("workspace");
    std::fs::create_dir_all(&workspace_root).map_err(|source| WorkspaceError::CreateWorkspace {
        path: workspace_root.clone(),
        source,
    })?;
    copy_repo_tree(&repo_root, &workspace_root)?;
    Ok(())
}

fn workspace_repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.ancestors().nth(3).map(Path::to_path_buf);
    root.ok_or(WorkspaceError::RepoRoot { manifest_dir })
}

fn copy_repo_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).map_err(|source| WorkspaceError::CreateDirectory {
        path: dst.to_path_buf(),
        source,
    })?;
    let entries = std::fs::read_dir(src).map_err(|source| WorkspaceError::ReadDirectory {
        path: src.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if should_skip_workspace_entry(&file_name) {
            continue;
        }
        let source_path = entry.path();
        let target_path = dst.join(file_name.as_ref());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_repo_tree(&source_path, &target_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| {
                    WorkspaceError::CreateDirectory {
                        path: parent.to_path_buf(),
                        source,
                    }
                })?;
            }
            std::fs::copy(&source_path, &target_path).map_err(|source| WorkspaceError::Copy {
                source_path: source_path.clone(),
                target_path: target_path.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

fn should_skip_workspace_entry(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".gents"
            | ".lake"
            | "target"
            | "node_modules"
            | "test-results"
            | "playwright-report"
            | ".next"
            | ".turbo"
            | "dist"
            | "build"
            | ".direnv"
    ) || name == ".env"
        || name.starts_with(".env.")
}
