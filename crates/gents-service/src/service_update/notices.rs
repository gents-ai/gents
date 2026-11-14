//! Runtime-scoped receipt observation. Publication goes through the existing
//! mailbox/session owner and never mutates transcript rows or completed tools.
use super::*;

/// Failure recovery rearms the native watcher with bounded backoff. Healthy
/// observation waits only on file events. Mailbox reconciliation is idempotent;
/// ConfigAccess still owns transaction-conflict retry, not this observer.
/// The root is created once, at start: a re-arm must not create it inside a
/// directory that replaced the home.
pub async fn watch_notices(runtime: gents::Gents, home: PathBuf) {
    let root = home.join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME);
    let created = tokio::task::spawn_blocking(move || files::private_dir(&root))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|created| created);
    if let Err(error) = created {
        tracing::warn!(%error, "managed update notice directory unavailable");
    }
    let mut backoff = Duration::from_secs(1);
    loop {
        if let Err(error) = observe(&runtime, &home).await {
            tracing::warn!(%error, "managed update observer will rearm; durable receipts remain available");
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn observe(runtime: &gents::Gents, home: &Path) -> Result<()> {
    let root = home.join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME);
    loop {
        // Observe only operation directories, not large backup/store trees.
        // Register before reading so creation and atomic replacement cannot
        // be lost between inspection and waiting.
        let mut changes = files::Changes::operations(&root).await?;
        if reconcile(runtime, &root).await? {
            // A failed publication, not a healthy-file poll. Re-arm even if
            // the producer has exited and no further file event will arrive.
            tokio::select! {
                result = changes.changed() => result?,
                _ = tokio::time::sleep(Duration::from_secs(5)) => {},
            }
        } else {
            changes.changed().await?;
        }
    }
}

async fn reconcile(runtime: &gents::Gents, root: &Path) -> Result<bool> {
    let mut deferred = false;
    let mut entries = tokio::fs::read_dir(root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
        else {
            continue;
        };
        if let Err(error) = publish_one(runtime, entry.path(), id).await {
            deferred = true;
            tracing::warn!(operation_id = %id, %error, "managed update notice deferred");
        }
    }
    Ok(deferred)
}

async fn publish_one(runtime: &gents::Gents, directory: PathBuf, id: uuid::Uuid) -> Result<()> {
    let marker = directory.join("notice-published.json");
    if tokio::fs::try_exists(directory.join("notice-unavailable.json")).await?
        || tokio::fs::try_exists(&marker).await?
        || !tokio::fs::try_exists(directory.join("receipt.json")).await?
    {
        return Ok(());
    }
    let receipt: Receipt = files::read_json_async(&directory.join("receipt.json")).await?;
    if receipt.node_did != runtime.node_did()
        || receipt.operation_id != id
        || !matches!(receipt.phase, Phase::Healthy | Phase::Failed)
    {
        return Ok(());
    }
    let (Some(session_id), Some(request_id)) = (receipt.session_id, receipt.request_id) else {
        return Ok(());
    };
    let lock_path = directory.join("notice.lock");
    let _lock = tokio::task::spawn_blocking(move || files::lock(&lock_path)).await??;
    let result = runtime
        .publish_runtime_notice(gents_session::session::mailbox::RuntimeNotice {
            source_id: format!("service-update:{id}"),
            session_id,
            request_id,
            title: if receipt.phase == Phase::Healthy {
                "Runtime update complete"
            } else {
                "Runtime update failed"
            }
            .into(),
            summary: format!("Update {id}: {}", receipt.message),
            failed: receipt.phase == Phase::Failed,
        })
        .await;
    match result {
        Ok(_) => {}
        Err(gents_session::session::mailbox::RuntimeNoticeError::Scope(error)) => {
            let unavailable = directory.join("notice-unavailable.json");
            let reason = error.to_string();
            tokio::task::spawn_blocking(move || files::write_json(&unavailable, &reason)).await??;
            return Ok(());
        }
        Err(gents_session::session::mailbox::RuntimeNoticeError::Unavailable(error)) => {
            return Err(error)
        }
        Err(
            error @ gents_session::session::mailbox::RuntimeNoticeError::MissingClaimProvenance,
        ) => return Err(error.into()),
    }
    tokio::task::spawn_blocking(move || files::write_json(&marker, &true)).await??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_rearm_does_not_create_service_updates_in_a_replacement_home() {
        let replacement = tempfile::tempdir().unwrap();
        let root = replacement
            .path()
            .join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME);
        let rearm =
            tokio::time::timeout(Duration::from_secs(2), files::Changes::operations(&root)).await;
        assert!(
            matches!(rearm, Ok(Err(_))),
            "a re-arm without its root fails"
        );
        assert!(!replacement
            .path()
            .join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME)
            .exists());
    }
}
