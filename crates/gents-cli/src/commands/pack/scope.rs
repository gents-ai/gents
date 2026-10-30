use crate::cli::*;
use anyhow::Result;
use gents::pack::PackManifest;
use std::collections::BTreeMap;

/// An uninitialized home has no node, so no runtime to wake: the next start
/// resolves the plugin store as it is.
pub(crate) async fn plugin_notification_scope(
    home: &std::path::Path,
) -> Result<Option<(gents::ConfigAccess, String)>> {
    if !gents::home::init_config_path(home).is_file() {
        return Ok(None);
    }
    resolve_scope_owner(&ScopeArgs {
        home: Some(home.to_owned()),
        graphql: None,
        node_did: None,
    })
    .await
    .map(Some)
}

pub(crate) async fn record_plugin_store_change(
    home: &std::path::Path,
    coordinate: &str,
    installed: Option<&gents::pack::PackIdentity>,
) -> Result<()> {
    if !gents::home::init_config_path(home).is_file() {
        return Ok(());
    }
    let (access, owner) = resolve_scope_owner(&ScopeArgs {
        home: Some(home.to_owned()),
        graphql: None,
        node_did: None,
    })
    .await?;
    let result =
        gents::pack::record_plugin_store_change(&access, &owner, home, coordinate, installed).await;
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    result.map_err(Into::into)
}

pub(crate) fn warn_on_unrecorded_removal(coordinate: &str, recorded: Result<()>) {
    if let Err(error) = recorded {
        tracing::warn!(
            coordinate,
            error = %error,
            "plugins removed, but the node was not told: a running runtime keeps the removed \
             tools listed (their calls fail) until it restarts or its configuration changes",
        );
    }
}

pub(super) async fn resolve_plugin_slot_owner(
    scope: &ScopeArgs,
    manifest: &PackManifest,
    requested: &BTreeMap<String, String>,
) -> Result<Option<String>> {
    if requested.is_empty() {
        return Ok(None);
    }
    for slot in requested.keys() {
        anyhow::ensure!(
            manifest
                .metadata
                .inference_slots
                .iter()
                .any(|declared| declared.name == *slot),
            "pack {} has no inference slot {slot:?}",
            manifest.name
        );
    }
    let (access, owner) = resolve_scope_owner(scope).await?;
    let result =
        gents::pack::preview_pack_inference_bindings(&access, manifest, &owner, requested).await;
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    result?;
    Ok(Some(owner))
}

pub(crate) async fn resolve_scope_owner(
    scope: &ScopeArgs,
) -> Result<(gents::config_client::ConfigAccess, String)> {
    let (access, _) =
        crate::resolve_config_access(scope.home.as_deref(), scope.graphql.as_deref()).await?;
    let owner = super::super::config::binding::resolve_target_node_did(
        scope.node_did.as_deref(),
        if scope.node_did.is_some() {
            None
        } else if scope.graphql.is_some() {
            Some(ManifestNodeDidBindingArg::Live)
        } else {
            Some(ManifestNodeDidBindingArg::Home)
        },
        scope.home.as_deref(),
        scope.graphql.as_deref(),
        Some(&access),
    )
    .await?;
    Ok((access, owner))
}
