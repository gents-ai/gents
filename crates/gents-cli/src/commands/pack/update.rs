//! `gents pack outdated` and `gents pack update`: installed packs against the
//! registry's latest versions.
//!
//! What is installed comes from the node's installation records; an update
//! is an ordinary install of the latest version, so the drift rules and
//! `--overwrite` / `--keep` apply exactly as they do to any install.

use anyhow::Result;
use serde_json::{json, Value};

use super::registry::{resolve_registry_url, RegistryClient};
use crate::cli::{PackInstallArgs, PackOutdatedArgs, PackUpdateArgs};
use crate::output_format::OutputFormat;

/// One installed pack and the latest version the registry has, if any.
async fn outdated_packs(
    scope: &crate::cli::GraphScopeArgs,
    registry: Option<&str>,
) -> Result<Vec<Value>> {
    let (access, owner) = super::resolve_scope_owner(scope).await?;
    let client = RegistryClient::new(resolve_registry_url(registry));
    let mut report = Vec::new();
    for pack in gents::pack::list_installed_packs(&access, &owner).await? {
        let (namespace, name) = super::split_namespace(&pack.coordinate);
        let latest = client
            .package(namespace, name)
            .await
            .ok()
            .and_then(|package| package["latest"].as_str().map(str::to_owned));
        report.push(json!({
            "pack": pack.coordinate,
            "installed": pack.version,
            "latest": latest,
            "outdated": latest.as_deref().is_some_and(|latest| latest != pack.version),
        }));
    }
    Ok(report)
}

pub(crate) async fn outdated(args: PackOutdatedArgs) -> Result<()> {
    let packs = outdated_packs(&args.scope, args.registry.as_deref()).await?;
    crate::print_json(&json!({ "packs": packs }))
}

pub(crate) async fn update(args: PackUpdateArgs) -> Result<()> {
    let packs = outdated_packs(&args.scope, args.registry.as_deref()).await?;
    let wanted: Vec<String> = packs
        .iter()
        .filter(|pack| pack["outdated"] == true)
        .filter_map(|pack| pack["pack"].as_str().map(str::to_owned))
        .filter(|coordinate| {
            args.package.as_deref().is_none_or(|package| {
                let (namespace, name) = super::split_namespace(package);
                *coordinate == format!("{namespace}/{name}")
            })
        })
        .collect();
    for coordinate in &wanted {
        super::install(PackInstallArgs {
            package: coordinate.clone(),
            bindings: None,
            inference_slots: Vec::new(),
            preview: false,
            scope: args.scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: args.registry.clone(),
            drift: args.drift,
        })
        .await?;
    }
    crate::print_json(&json!({ "updated": wanted }))
}
