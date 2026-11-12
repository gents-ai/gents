use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use gents_pack::pack::{PackChangeError, PackKind, RemoveReport};
use serde_json::{json, Value};

use crate::cli::PackRemoveArgs;

pub(crate) async fn remove(args: PackRemoveArgs) -> Result<(), PackChangeError> {
    let (namespace, name) = gents_pack::pack::registry::split_pack_coordinate(&args.package);
    let coordinate = format!("{namespace}/{name}");
    let home = gents_service::home_state::resolve_home_dir(args.scope.home.as_deref());

    if args.scope.graphql.is_none() {
        if let Some(record) = gents_pack::pack::read_home_install(&home, &coordinate)? {
            if matches!(record.kind, PackKind::Assets | PackKind::Plugins) {
                if args.scope.node_did.is_some() {
                    return Err(anyhow::anyhow!(
                        "asset and plugins packs are removed locally with --home; identity flags do not apply"
                    )
                    .into());
                }
                let removed = remove_home_install(&home, &coordinate).await?;
                return Ok(crate::print_json(
                    &json!({ "pack": coordinate, "removed": removed }),
                )?);
            }
        }
        if !gents_store::local::home::init_config_path(&home).is_file() {
            return Err(
                anyhow::anyhow!("{coordinate} is not installed in {}", home.display()).into(),
            );
        }
    }

    let (access, owner) = super::resolve_scope_owner(&args.scope).await?;
    let result = async {
        let report = gents_pack::pack::remove_pack(
            &access,
            &owner,
            &coordinate,
            args.drift.policy(),
            &gents_inference::resolved::ProviderProfilePolicy,
        )
        .await?;
        let released = if args.scope.graphql.is_none() {
            gents_pack::pack::home_removal::release_documents_pack(&home, Some(&access), &report)
                .await?
        } else {
            Default::default()
        };
        let removed = augment_report(&report, &released)?;
        Ok::<_, PackChangeError>(json!({ "pack": coordinate, "owner": owner, "removed": removed }))
    }
    .await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    Ok(crate::print_json(&result?)?)
}

fn augment_report(
    report: &RemoveReport,
    released: &gents_pack::pack::home_removal::DocumentsRelease,
) -> Result<Value> {
    let mut value = serde_json::to_value(report).context("encoding the remove report")?;
    let object = value
        .as_object_mut()
        .context("remove report must serialize as an object")?;
    object.insert("plugins".to_owned(), json!(released.plugins));
    if let Some(dependencies) = object.get_mut("dependencies").and_then(Value::as_array_mut) {
        for (index, dependency) in dependencies.iter_mut().enumerate() {
            let dependency = dependency
                .as_object_mut()
                .context("dependency report must serialize as an object")?;
            let plugins = released.dependency_plugins.get(index).cloned();
            dependency.insert("plugins".to_owned(), json!(plugins.unwrap_or_default()));
        }
    }
    object.insert("plugin_bytes".to_owned(), json!(released.plugin_bytes));
    object.insert("archives".to_owned(), json!(released.archives));
    Ok(value)
}

#[cfg(test)]
pub(super) struct RemovalTestPause {
    pub(super) home: std::path::PathBuf,
    pub(super) reached: tokio::sync::Notify,
    pub(super) release: tokio::sync::Notify,
}

#[cfg(test)]
pub(super) static REMOVAL_TEST_PAUSE: std::sync::Mutex<Option<std::sync::Arc<RemovalTestPause>>> =
    std::sync::Mutex::new(None);

/// This preflight observes one document snapshot. The post-removal
/// PackInstallation notification remains required for configuration changes
/// racing the filesystem operation.
pub(crate) async fn ensure_home_plugins_unreferenced(
    home: &Path,
    plugins: &BTreeSet<String>,
    guard: &gents_pack::plugin::store::StoreGuard,
) -> Result<()> {
    if plugins.is_empty() {
        return Ok(());
    }
    if !gents_store::local::home::init_config_path(home).is_file() {
        return Err(gents_pack::pack::PackRemovalError::NodeUnobserved.into());
    }
    #[cfg(test)]
    {
        let pause = REMOVAL_TEST_PAUSE
            .lock()
            .unwrap()
            .as_ref()
            .filter(|pause| pause.home == home)
            .cloned();
        if let Some(pause) = pause {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }
    let (access, owner) = super::resolve_home_owner_under_plugin_store(home, guard).await?;
    let result = gents_pack::pack::ensure_plugins_unreferenced(&access, &owner, plugins).await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    drop(access);
    result
}

pub(crate) async fn remove_home_install(home: &Path, coordinate: &str) -> Result<Value> {
    let lock_home = home.to_owned();
    let guard =
        tokio::task::spawn_blocking(move || gents_pack::plugin::store::try_lock_store(&lock_home))
            .await??;
    remove_home_install_with_guard(home, coordinate, guard).await
}

pub(crate) async fn remove_home_install_with_guard(
    home: &Path,
    coordinate: &str,
    guard: gents_pack::plugin::store::StoreGuard,
) -> Result<Value> {
    guard.for_home(home)?;
    let record = gents_pack::pack::read_home_install(home, coordinate)?
        .with_context(|| format!("{coordinate} is not installed"))?;
    let (namespace, _) = gents_pack::pack::registry::split_pack_coordinate(coordinate);
    let plugins = record
        .plugins
        .iter()
        .map(|plugin| format!("{namespace}/{}", plugin.name))
        .collect();
    ensure_home_plugins_unreferenced(home, &plugins, &guard).await?;
    let removal =
        gents_pack::pack::home_removal::remove_checked(guard, home, coordinate, record, None)
            .await?;
    if !removal.plugins.is_empty() {
        super::warn_on_unrecorded_removal(
            coordinate,
            super::record_plugin_retirement(home, coordinate).await,
        );
    }
    Ok(json!({
        "assets": removal.assets,
        "plugins": removal.plugins,
        "retained": removal.retained,
        "plugin_bytes": removal.plugin_bytes,
        "archives": removal.archives,
    }))
}

#[cfg(test)]
mod tests;
