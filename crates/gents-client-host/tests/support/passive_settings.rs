use super::Attachment;
use anyhow::{ensure, Context, Result};
use gents_desktop_core::client::DesktopPaths;
use serde_json::json;
use std::path::Path;

/// The source keeps its loaded signer, but neither a new client read nor a
/// reattached client can obtain operator authority from the absent key file.
pub async fn exercise(
    mut client: Attachment,
    id: &mut u32,
    paths: &DesktopPaths,
    host_binary: &Path,
    operator_key: &Path,
    node: &str,
    installed: bool,
) -> Result<Attachment> {
    let held_key = operator_key.with_extension("held-by-metadata-fixture");
    tokio::fs::rename(operator_key, &held_key).await?;
    let result = async {
        if installed {
            installed_editor(paths, node).await?;
            ensure!(
                !operator_key.exists(),
                "the installed editor recreated the operator key"
            );
            ensure!(
                client
                    .call(*id, "client_runtime_is_local", json!({"nodeDid":node}))
                    .await
                    == false,
                "the installed editor replaced the enrolled route with operator authority"
            );
            *id += 1;
            return Ok(client);
        }
        for connection in 0..2 {
            if connection > 0 {
                client.close().await;
                client = Attachment::start_with_binary(paths, host_binary).await;
            }
            ensure!(
                !operator_key.exists(),
                "operator key became available during passive reads"
            );
            ensure!(
                client
                    .call(*id, "client_runtime_is_local", json!({"nodeDid":node}))
                    .await
                    == false,
                "passive metadata must use the enrolled route, not local operator authority"
            );
            *id += 1;
            for _ in 0..2 {
                let settings = client
                    .try_call(*id, "client_inference_setup_state", json!({"nodeDid":node}))
                    .await?;
                *id += 1;
                ensure!(
                    settings["nodeDid"] == node && settings["node"]["nodeDid"] == node,
                    "model settings crossed runtime identity"
                );
                let backends = settings["inferenceBackends"]
                    .as_array()
                    .context("enrolled model backends")?;
                ensure!(
                    !backends.is_empty()
                        && settings["inferenceProfiles"]
                            .as_array()
                            .is_some_and(|profiles| !profiles.is_empty()),
                    "passive settings replaced configured inference with an empty setup"
                );
                for backend in backends {
                    ensure!(
                        backend.get("apiKey").is_none()
                            && backend.get("auth").is_none()
                            && backend["apiKeyEnvVar"].is_null(),
                        "model metadata exposed raw credentials"
                    );
                }
                ensure!(
                    !operator_key.exists(),
                    "model settings recreated the operator key"
                );
            }
        }
        Ok(client)
    }
    .await;
    tokio::fs::rename(&held_key, operator_key)
        .await
        .context("restoring the disposable runtime key")?;
    result
}

async fn installed_editor(paths: &DesktopPaths, node: &str) -> Result<()> {
    let extension = std::path::PathBuf::from(
        std::env::var_os("GENTS_VSCODE_ROOT").context("set GENTS_VSCODE_ROOT")?,
    );
    ensure!(
        extension.is_absolute(),
        "the extension fixture path must be absolute"
    );
    let root = paths
        .root()
        .parent()
        .context("disposable settings fixture root")?;
    let output = tokio::process::Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/editor_passive_settings.mjs"
        ))
        .env("GENTS_VSCODE_ROOT", &extension)
        .arg(root)
        .arg(paths.root())
        .arg(node)
        .kill_on_drop(true)
        .output()
        .await
        .context("running installed model-settings observer")?;
    tokio::fs::write(root.join("editor-passive-settings.log"), &output.stderr).await?;
    ensure!(
        output.status.success(),
        "installed model-settings observer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let evidence: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(root.join("installed-passive-settings.json")).await?,
    )?;
    ensure!(
        evidence["draftPreserved"] == true,
        "the installed observer did not preserve its draft"
    );
    ensure!(
        evidence["observations"]
            .as_array()
            .is_some_and(|rows| rows.len() >= 2),
        "the installed observer did not qualify both settings reads"
    );
    Ok(())
}
