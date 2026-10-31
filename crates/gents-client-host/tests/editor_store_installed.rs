#![cfg(all(target_os = "macos", feature = "editor-e2e"))]

use anyhow::{ensure, Context, Result};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents::store::local::store_key::{StoreEncryption, StoreKeyCustody};
use gents_desktop_bridge::host::{local, protocol};
use gents_desktop_core::client::DesktopPaths;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use tokio_util::codec::LengthDelimitedCodec;

async fn script(extension: &Path, root: &Path, name: &str) -> Result<()> {
    let mut command = Command::new("node");
    command
        .arg(extension.join("tests/editorCachedStartup.mjs"))
        .env("GENTS_VSCODE_CACHE_PHASE", name)
        .env("GENTS_VSCODE_STARTUP_ROOT", root)
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(90), command.output());
    tokio::pin!(output);
    let ready = async {
        let paths = DesktopPaths::from_root(root.join("client"));
        tokio::time::timeout(Duration::from_secs(30), async {
            while !root.join(format!("cache-{name}-ui.json")).exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let stream = loop {
                match local::connect(&paths).await {
                    Ok(stream) => break stream,
                    Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
                }
            };
            let mut frames = LengthDelimitedCodec::builder()
                .max_frame_length(protocol::MAX_FRAME_BYTES)
                .new_framed(stream);
            frames
                .send(Bytes::from(serde_json::to_vec(
                    &json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}),
                )?))
                .await?;
            let hello: Value = serde_json::from_slice(
                &frames.next().await.context("cached installation hello")??,
            )?;
            ensure!(
                hello["kind"] == "hello" && hello["version"] == protocol::PROTOCOL_VERSION,
                "the cached installation did not complete native startup"
            );
            tokio::fs::write(root.join(format!("cache-{name}-release.json")), b"{}").await?;
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("native cached startup deadline")?
    };
    tokio::select! {
        result = &mut output => {
            let result = result.context("installed cache observer deadline")??;
            anyhow::bail!("{name} observer ended before native readiness: {}", String::from_utf8_lossy(&result.stderr));
        }
        result = ready => {
            if let Err(error) = result {
                // Release the UI observer so its finally block can close the
                // disposable editor; the native failure remains authoritative.
                tokio::fs::write(root.join(format!("cache-{name}-release.json")), b"{}").await?;
                let _ = output.await;
                return Err(error);
            }
        },
    }
    let result = output
        .await
        .context("installed cache observer deadline")??;
    ensure!(
        result.status.success(),
        "{name} failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

/// Retire only the identified installation in this test's disposable home.
/// The native retirement owner drains its attachments before the store closes.
async fn retire(paths: &DesktopPaths) -> Result<()> {
    let stream = match local::connect(paths).await {
        Ok(stream) => stream,
        Err(_) => return Ok(()),
    };
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut frames = LengthDelimitedCodec::builder()
            .max_frame_length(protocol::MAX_FRAME_BYTES)
            .new_framed(stream);
        frames
            .send(Bytes::from(serde_json::to_vec(
                &json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}),
            )?))
            .await?;
        let hello: Value =
            serde_json::from_slice(&frames.next().await.context("installation hello")??)?;
        let build = hello["installationBuild"]
            .as_str()
            .filter(|build| !build.is_empty())
            .context("identified disposable installation")?;
        frames
            .send(Bytes::from(serde_json::to_vec(
                &json!({"kind":"retire", "id":1, "expectedBuild":build}),
            )?))
            .await?;
        let mut acknowledged = false;
        while let Some(frame) = frames.next().await {
            let message: Value = serde_json::from_slice(&frame?)?;
            if message["kind"] == "response" && message["id"] == 1 {
                ensure!(
                    message["error"].is_null(),
                    "disposable installation refused retirement"
                );
                acknowledged = true;
            }
        }
        ensure!(
            acknowledged,
            "disposable installation retirement was not acknowledged"
        );
        gents::store::local::home::wait_for_store_release(paths.root(), paths.node_data_dir())
            .await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("retiring the disposable installation")??;
    Ok(())
}

async fn remove_fixture_key(paths: &DesktopPaths) -> Result<()> {
    if !paths.store_encryption_path().exists() {
        return Ok(());
    }
    let record: StoreEncryption =
        serde_json::from_slice(&tokio::fs::read(paths.store_encryption_path()).await?)?;
    ensure!(
        matches!(record.custody, StoreKeyCustody::MacosKeychain { .. }),
        "the fixture must retain macOS custody"
    );
    let StoreKeyCustody::MacosKeychain { keychain_label } = record.custody else {
        unreachable!()
    };
    ensure!(
        keychain_label.strip_prefix("store-").is_some_and(
            |suffix| suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        ),
        "invalid fixture item label"
    );
    // Deleting this generated item needs no password read. The general custody
    // API reads before deletion and can require access granted only to its creator.
    let deleted = Command::new("/usr/bin/security")
        .args([
            "delete-generic-password",
            "-s",
            "com.source-inc.gents.store-key",
            "-a",
            &keychain_label,
        ])
        .output()
        .await?;
    ensure!(
        deleted.status.success() || deleted.status.code() == Some(44),
        "fixture credential cleanup failed: {}",
        String::from_utf8_lossy(&deleted.stderr)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installed_editor_reopens_its_encrypted_cache_after_the_native_item_is_removed(
) -> Result<()> {
    let extension = std::path::PathBuf::from(
        std::env::var_os("GENTS_VSCODE_ROOT").context("set GENTS_VSCODE_ROOT")?,
    );
    ensure!(
        extension.is_absolute(),
        "the extension fixture path must be absolute"
    );
    let root = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(root.path().join("client"));
    let result = async {
        script(&extension, root.path(), "initial").await?;
        let custody = tokio::fs::read(paths.store_encryption_path()).await?;
        let principal = tokio::fs::read(paths.principal_metadata_path()).await?;
        ensure!(
            !paths.store_key_path().exists(),
            "system custody created a plaintext key file"
        );
        retire(&paths).await?;
        remove_fixture_key(&paths).await?;
        script(&extension, root.path(), "reopen").await?;
        ensure!(
            tokio::fs::read(paths.store_encryption_path()).await? == custody,
            "cold reopen changed the custody record"
        );
        ensure!(
            tokio::fs::read(paths.principal_metadata_path()).await? == principal,
            "cold reopen changed the client identity"
        );
        ensure!(
            !paths.store_key_path().exists(),
            "cold reopen substituted plaintext custody"
        );
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let stopped = retire(&paths).await;
    let cleaned = remove_fixture_key(&paths).await;
    let failures = [result, stopped, cleaned]
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
    if failures.is_empty() {
        return Ok(());
    }
    let retained = root.keep();
    anyhow::bail!(
        "{}; disposable fixture retained at {}",
        failures.join("\n"),
        retained.display()
    )
}
