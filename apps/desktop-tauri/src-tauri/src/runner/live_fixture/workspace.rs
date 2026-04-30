use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

pub(super) fn seed_runner_agent_home(
    agent_home: &Path,
    agent_name: &str,
    agent_did: &str,
    peer_id: &str,
    listen_address: &str,
) -> Result<()> {
    std::fs::create_dir_all(agent_home)?;
    std::fs::write(
        agent_home.join("init.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "agent_name": agent_name,
            "agent_did": agent_did,
        }))?,
    )?;
    std::fs::write(
        agent_home.join("runtime.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "graphql": "",
            "agent_name": agent_name,
            "agent_did": agent_did,
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
    std::fs::create_dir_all(&workspace_root)
        .with_context(|| format!("creating seeded workspace {}", workspace_root.display()))?;
    copy_repo_tree(&repo_root, &workspace_root)?;
    Ok(())
}

fn workspace_repo_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to locate repo root from {}", manifest_dir.display()))
}

fn copy_repo_tree(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
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
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            std::fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "copying {} -> {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn should_skip_workspace_entry(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".next" | ".turbo" | "dist" | "build" | ".direnv"
    )
}
