use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use futures::StreamExt;
use gents::store_key::{StoreEncryption, StoreKeyCustodyChoice};
use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
use serde_json::Value;
use tokio::process::{Child, Command};
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};

pub async fn exercise() -> Result<()> {
    let previous = PathBuf::from(
        std::env::var_os("GENTS_PREVIOUS_CLIENT_HOST")
            .context("provide the packaged predecessor client host")?,
    );
    ensure!(previous.is_absolute() && previous.is_file());
    let current = super::support::native_binary("gents-client-host")?;
    let directory = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(directory.path().join("client"));
    paths.ensure_root_dirs().await?;
    prepare_file_store(&paths).await?;
    let mut old = start(&previous, &paths).await?;
    let mut client = super::support::Attachment::start_with_binary(&paths, &previous).await;
    let snapshot = client.snapshot(1).await;
    let did = snapshot["client"]["clientDid"]
        .as_str()
        .context("original client DID")?
        .to_owned();
    client.close().await;
    super::stop(&mut old).await?;
    let before = schema(&paths, &did).await?;
    ensure!(
        before.version_id == "bafyreibbbdmb7au32traw5ufp6rqytb3jv63bmjflykfhize2u6bwvqiwe",
        "the supplied predecessor did not create the reported source lineage"
    );
    let key = blake3::hash(&tokio::fs::read(paths.store_key_path()).await?);
    let custody = tokio::fs::read(paths.store_encryption_path()).await?;
    let mut updated = start(&current, &paths).await?;
    let mut client = super::support::Attachment::start_with_binary(&paths, &current).await;
    let snapshot = client.snapshot(2).await;
    ensure!(
        snapshot["client"]["clientDid"] == did,
        "the upgrade replaced the client identity"
    );
    client.close().await;
    super::stop(&mut updated).await?;
    ensure!(
        blake3::hash(&tokio::fs::read(paths.store_key_path()).await?) == key,
        "the fixture key changed"
    );
    ensure!(
        tokio::fs::read(paths.store_encryption_path()).await? == custody,
        "the fixture custody changed"
    );
    let after = schema(&paths, &did).await?;
    ensure!(
        before.collection_id == after.collection_id,
        "the upgrade replaced the collection identity"
    );
    for field in before.fields {
        let retained = after
            .fields
            .iter()
            .find(|candidate| candidate.name == field.name)
            .context("retained inference field")?;
        ensure!(
            retained.id == field.id
                && retained.kind == field.kind
                && retained.immutable == field.immutable,
            "the upgrade changed an existing field"
        );
    }
    for name in [
        "session_id",
        "model_name",
        "profile_id",
        "cache_creation_input_tokens",
        "reasoning_output_tokens",
        "provider_total_tokens",
        "usage_completeness",
    ] {
        ensure!(
            after.fields.iter().any(|field| field.name == name),
            "the upgrade omitted {name}"
        );
    }
    let mut restarted = start(&current, &paths).await?;
    let mut client = super::support::Attachment::start_with_binary(&paths, &current).await;
    ensure!(client.snapshot(3).await["client"]["clientDid"] == did);
    client.close().await;
    super::stop(&mut restarted).await?;
    Ok(())
}

pub(super) async fn prepare_file_store(paths: &DesktopPaths) -> Result<()> {
    let prepare = paths.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let record = StoreEncryption::prepare(
            StoreKeyCustodyChoice::File,
            prepare.store_key_path(),
            prepare.node_data_dir(),
        )?;
        std::fs::write(
            prepare.store_encryption_path(),
            serde_json::to_vec(&record)?,
        )?;
        record.initialize(prepare.store_key_path(), prepare.node_data_dir())?;
        Ok(())
    })
    .await??;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn replace_when_requested(
    root: &Path,
    paths: &DesktopPaths,
    did: &str,
    host: &mut Child,
    current: &Path,
    runtime: &mut Child,
    current_cli: &Path,
    node_home: &Path,
    port: u16,
    node_did: &str,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !tokio::fs::try_exists(root.join("retained-upgrade-requested")).await? {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("old installed editor did not finish its retained-history checkpoint")??;
    let key = blake3::hash(&tokio::fs::read(paths.store_key_path()).await?);
    let custody = tokio::fs::read(paths.store_encryption_path()).await?;
    super::stop(host).await?;
    let before = schema(paths, did).await?;
    ensure!(
        before.version_id == "bafyreibbbdmb7au32traw5ufp6rqytb3jv63bmjflykfhize2u6bwvqiwe",
        "the installed baseline did not retain the reported source lineage"
    );
    super::stop(runtime).await?;
    let log_path = root.join("runtime-upgrade.log");
    let output = tokio::fs::File::create(&log_path).await?.into_std().await;
    let errors = tokio::fs::File::create(root.join("runtime-upgrade.err"))
        .await?
        .into_std()
        .await;
    let mut next_runtime = super::runtime_server_command(current_cli, root, node_home, port)
        .stdin(Stdio::null())
        .stdout(Stdio::from(output))
        .stderr(Stdio::from(errors))
        .kill_on_drop(true)
        .spawn()?;
    let ready = super::wait_ready(&mut next_runtime, &log_path).await?;
    ensure!(
        ready["node_did"] == node_did,
        "the update replaced the runtime identity"
    );
    let status = super::wait_for_status(&mut next_runtime, current_cli, node_home, port).await?;
    ensure!(status["node_did"] == node_did && status["readiness_status"] == "ready");
    *runtime = next_runtime;
    *host = start(current, paths).await?;
    ensure!(
        blake3::hash(&tokio::fs::read(paths.store_key_path()).await?) == key,
        "the paired update replaced the client key"
    );
    ensure!(
        tokio::fs::read(paths.store_encryption_path()).await? == custody,
        "the paired update replaced client key custody"
    );
    tokio::fs::write(
        root.join("retained-upgrade-retired"),
        serde_json::to_vec(&serde_json::json!({
            "sourceSchemaVersion": before.version_id,
            "clientKeyPreserved": true,
            "clientCustodyPreserved": true,
            "runtimeIdentityPreserved": true,
        }))?,
    )
    .await?;
    Ok(())
}

async fn start(binary: &Path, paths: &DesktopPaths) -> Result<Child> {
    let mut child = Command::new(binary)
        .env("GENTS_HOME", paths.root().join("unused-runtime"))
        .env("GENTS_DESKTOP_HOME", paths.root())
        .env_remove("GENTS_SYSTEM_LOG")
        .args(["--serve", "--editor-store", "--home"])
        .arg(paths.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut output = FramedRead::new(
        child.stdout.take().context("bootstrap output")?,
        LengthDelimitedCodec::new(),
    );
    let bytes = tokio::time::timeout(Duration::from_secs(30), output.next())
        .await
        .context("client bootstrap deadline")?
        .context("client closed bootstrap")??;
    let ready: Value = serde_json::from_slice(&bytes)?;
    ensure!(ready["kind"] == "ready", "client startup failed: {ready}");
    Ok(child)
}

async fn schema(paths: &DesktopPaths, did: &str) -> Result<defra_node::CollectionVersion> {
    let prepare = paths.clone();
    let key = tokio::task::spawn_blocking(move || -> Result<_> {
        let record: StoreEncryption =
            serde_json::from_slice(&std::fs::read(prepare.store_encryption_path())?)?;
        record.load(prepare.store_key_path(), prepare.node_data_dir())
    })
    .await??;
    let identity = PrincipalIdentity::load_or_create(paths).await?;
    ensure!(identity.did() == did, "the retained identity changed");
    let node = key
        .encrypt(defra_node::EmbeddedNode::builder().data_path(paths.node_data_dir()))
        .with_node_identity_did(did.to_owned())
        .build()
        .await?;
    let schema = node
        .get_collection("InferenceCall")?
        .context("inference collection")?;
    node.shutdown().await;
    Ok(schema)
}
