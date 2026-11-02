use super::scope::resolve_plugin_slot_owner;
use super::{
    parse_inference_slot_bindings, plugin_notification_scope, resolve_pack_source,
    resolve_scope_owner, wire, PackSource,
};
use crate::cli::*;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use gents::pack::{PackKind, PackManifest};
use serde_json::json;

/// The documents pack install: the CLI's single site that writes a documents
/// pack and its dependencies. `explicit` is false only when the pack is
/// installed as another pack's dependency.
pub(crate) async fn install_documents_pack(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
) -> Result<serde_json::Value> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let dependencies: Vec<PackSource> =
        futures_util::stream::iter(pack.manifest().metadata.dependencies.iter())
            .map(|coordinate| resolve_pack_source(coordinate, args.registry.as_deref(), &home))
            .buffered(4)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
    if let gents::store::config_client::ConfigAccess::Graphql(endpoint) = access {
        let mut needs_runtime_store = false;
        if args.scope.graphql.is_some() {
            for source in std::iter::once(pack).chain(dependencies.iter()) {
                if !source.manifest().metadata.plugins.is_empty() {
                    needs_runtime_store = true;
                    break;
                }
                let config = gents::pack::load_pack_config(
                    source.manifest(),
                    &gents::pack::PackInstallOptions {
                        node_did: owner.to_owned(),
                    },
                    &|path| Ok(source.asset(path)?.to_vec()),
                    &|name| std::env::var(name).ok(),
                )?;
                if config.tools.iter().any(|tools| {
                    tools
                        .integrations
                        .as_ref()
                        .and_then(|integrations| integrations.plugins.as_ref())
                        .is_some_and(|plugins| !plugins.is_empty())
                }) {
                    needs_runtime_store = true;
                    break;
                }
            }
        }
        if needs_runtime_store {
            return crate::http::pack_install::install_remote(
                endpoint,
                owner,
                pack,
                &dependencies,
                args,
                explicit,
            )
            .await;
        }
    }
    install_documents_pack_resolved(access, owner, pack, args, explicit, dependencies).await
}

pub(crate) async fn install_documents_pack_resolved(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
    dependencies: Vec<PackSource>,
) -> Result<serde_json::Value> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let requested = parse_inference_slot_bindings(&args.inference_slots)?;
    let temp = tempfile::tempdir()?;
    gents::pack::materialize::materialize(pack.archive(), temp.path())?;
    let (mut authored, mut report) = crate::desired_state::load_manifest_root(temp.path());
    if authored.is_none() {
        (authored, report) =
            crate::desired_state::load_manifest_root_for_owner(temp.path(), Some(owner));
    }
    anyhow::ensure!(
        authored.is_some(),
        "invalid pack configuration: {:?}",
        report.errors
    );
    let mut authored = authored.expect("checked pack configuration");
    super::super::config::binding::rebind_manifest_to_node(
        &mut authored,
        owner,
        args.force_rebind_concrete_did,
    )?;
    let prepared = gents::pack::prepare_document_pack_install(
        access,
        owner,
        pack.manifest(),
        &authored,
        &requested,
        &dependencies
            .iter()
            .map(PackSource::archive)
            .collect::<Vec<_>>(),
        gents::pack::InstalledVia::Operator,
        args.scope.graphql.is_none().then_some(home.as_path()),
        Some(pack.archive()),
        &|name| std::env::var(name).ok(),
    )
    .await?;
    let inference = prepared.inference.clone();
    let dependency_inference = prepared.dependency_inference.clone();
    let origin_tag = gents::pack::pack_origin_tag(&pack.manifest().name)?;
    if args.preview {
        return Ok(json!({
            "pack": pack.manifest().name,
            "source": pack.label(),
            "digest": pack.digest(),
            "owner": owner,
            "inference": inference,
            "dependency_inference": dependency_inference,
            "origin_tag": origin_tag,
            "dependencies": pack.manifest().metadata.dependencies,
            "would_write": false,
        }));
    }
    // A dependency installed on its own behalf (`explicit` false) writes no
    // home record, and a remote node has no local home to record in.
    let record_home = explicit && args.scope.graphql.is_none();
    // Every pack that ships an asset class is materialized before any
    // write; the leases are held until the home record is written.
    let mut cache_leases = Vec::new();
    let mut materialized_assets = String::new();
    let mut dependency_assets = Vec::new();
    if record_home {
        for dependency in &dependencies {
            if let Some((assets, lease)) =
                gents::pack::home_install::materialize_asset_pack(&home, dependency.archive())?
            {
                dependency_assets.push((
                    format!(
                        "{}/{}",
                        dependency.manifest().metadata.namespace,
                        dependency.manifest().name
                    ),
                    assets,
                ));
                cache_leases.push(lease);
            }
        }
        if let Some((assets, lease)) =
            gents::pack::home_install::materialize_asset_pack(&home, pack.archive())?
        {
            materialized_assets = assets;
            cache_leases.push(lease);
        }
    }
    let dependency_coordinates: Vec<String> = dependencies
        .iter()
        .map(|dependency| {
            format!(
                "{}/{}",
                dependency.manifest().metadata.namespace,
                dependency.manifest().name
            )
        })
        .collect();
    gents::pack::apply_prepared_dependency_schemas(access, &prepared).await?;
    let schemas = super::super::schema::apply_pack_schemas_if_present(access, temp.path())
        .await
        .context("pack install schemas")?;
    let (apply, plugins) = gents::pack::install_prepared_document_graph(
        access,
        owner,
        pack.archive(),
        &prepared,
        args.drift.policy(),
        args.scope.graphql.is_none().then_some(home.as_path()),
        args.grant_authority,
        explicit,
    )
    .await?;
    let root_coordinate = format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    );
    let mut identity = gents::pack::PackIdentity::new(
        pack.manifest(),
        pack.digest(),
        plugins
            .iter()
            .filter(|plugin| {
                plugin.owner_pack_coordinate.as_deref() == Some(root_coordinate.as_str())
            })
            .map(|plugin| gents::pack::InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
    );
    identity.dependencies = dependency_coordinates;
    identity.explicit = explicit;
    if record_home {
        gents::pack::record_after_install(
            &home,
            gents::pack::HomePackInstall {
                coordinate: identity.coordinate.clone(),
                version: identity.version.clone(),
                digest: identity.digest.clone(),
                kind: PackKind::Documents,
                assets: materialized_assets,
                dependency_assets,
                plugins: identity.plugins.clone(),
                ux: pack.manifest().metadata.ux.clone(),
                installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                installed_via: Some(gents::pack::InstalledVia::Operator),
            },
            gents::pack::InstalledVia::Operator,
            explicit,
        )
        .context("the pack documents were written, but the home install record was not")?;
    }
    Ok(json!({
        "pack": pack.manifest().name,
        "source": pack.label(),
        "digest": pack.digest(),
        "owner": owner,
        "inference": inference,
        "dependency_inference": dependency_inference,
        "origin_tag": origin_tag,
        "dependencies": pack.manifest().metadata.dependencies,
        "schemas": schemas,
        "apply": apply,
    }))
}

/// Refuses, before any write, a pack whose content digest is not the one the
/// operator reviewed with `gents pack show`.
pub(super) fn check_expected_digest(expected: Option<&str>, actual: &str) -> Result<()> {
    match expected {
        Some(expected) if expected != actual => anyhow::bail!(
            "the pack's digest is {actual}, not the expected {expected}. Nothing was installed. \
             Next: gents pack show ./dir"
        ),
        _ => Ok(()),
    }
}

/// `--wire` acts on plugins this install places, so a pack that cannot ship
/// one refuses it before any write.
pub(super) fn check_wire_target(manifest: &PackManifest, wire: &[String]) -> Result<()> {
    if wire.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        matches!(
            manifest.metadata.kind,
            PackKind::Documents | PackKind::Plugins
        ),
        "--wire applies only to documents and plugins packs; {} is not one",
        manifest.name
    );
    anyhow::ensure!(
        !manifest.metadata.plugins.is_empty(),
        "--wire needs a pack that ships a plugin; {} ships none",
        manifest.name
    );
    Ok(())
}

/// Wires each plugin `pack` shipped into each of `agents`, agent by agent,
/// through the same writer as `gents pack wire`.
pub(crate) async fn wire_pack_plugins(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    home: &std::path::Path,
    pack: &PackSource,
    agents: &[String],
) -> Result<Vec<serde_json::Value>> {
    let namespace = &pack.manifest().metadata.namespace;
    let mut wired = Vec::new();
    for agent in agents {
        for plugin in &pack.manifest().metadata.plugins {
            let request = wire::WireRequest {
                plugin: format!("{namespace}/{}", plugin.name),
                agent: agent.clone(),
                targets_from: Vec::new(),
                remove: false,
                preview: false,
            };
            wired.push(wire::wire_in(access, owner, home, &request).await?);
        }
    }
    Ok(wired)
}

/// Offline command access owns its node and store claim. DefraDB background
/// tasks may retain the database after a handle drops, so shutdown must finish
/// before this command releases its access. HTTP access has no owned node here.
pub(super) async fn install_documents_command(
    access: gents::store::config_client::ConfigAccess,
    pack: &PackSource,
    args: &PackInstallArgs,
) -> Result<()> {
    let result = async {
        let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
        let bind_mode = if args.scope.graphql.is_some() {
            Some(ManifestNodeDidBindingArg::Live)
        } else {
            Some(ManifestNodeDidBindingArg::Home)
        };
        let owner = super::super::config::binding::resolve_target_node_did(
            args.scope.node_did.as_deref(),
            if args.scope.node_did.is_some() {
                None
            } else {
                bind_mode
            },
            args.scope.home.as_deref(),
            args.scope.graphql.as_deref(),
            Some(&access),
        )
        .await?;
        let mut output = install_documents_pack(&access, &owner, pack, args, args.explicit).await?;
        if !args.wire.is_empty() && !args.preview && args.scope.graphql.is_none() {
            output["wired"] = wire_pack_plugins(&access, &owner, &home, pack, &args.wire)
                .await?
                .into();
        }
        Ok::<_, anyhow::Error>(output)
    }
    .await;
    if let Some(node) = access.local_node() {
        node.shutdown().await;
    }
    crate::print_json(&result?)
}

pub(crate) async fn install(args: PackInstallArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let pack = resolve_pack_source(&args.package, args.registry.as_deref(), &home).await?;
    tracing::info!(package = %args.package, source = %pack.describe(), "resolved pack");
    check_expected_digest(args.expect_digest.as_deref(), pack.digest())?;
    check_wire_target(pack.manifest(), &args.wire)?;
    args.output.ensure_supported(
        "pack install",
        &[crate::cli::output_format::OutputFormat::Json],
    )?;
    match pack.manifest().metadata.kind {
        PackKind::Documents => {
            let (access, _) = crate::resolve_config_access(
                args.scope.home.as_deref(),
                args.scope.graphql.as_deref(),
            )
            .await?;
            install_documents_command(access, &pack, &args).await
        }
        PackKind::Assets | PackKind::Plugins => {
            let requested = parse_inference_slot_bindings(&args.inference_slots)?;
            anyhow::ensure!(
                args.scope.graphql.is_none()
                    && (args.scope.node_did.is_none() || !requested.is_empty())
                    && !args.force_rebind_concrete_did,
                "asset and plugins packs install locally with --home; identity flags do not apply"
            );
            if args.preview {
                return crate::print_json(&json!({
                    "pack": pack.manifest().name,
                    "source": pack.label(),
                    "digest": pack.digest(),
                    "installed_plugins": pack.manifest().metadata.plugins,
                    "ux_plugins": pack.manifest().metadata.ux,
                    "inference_slots": pack.manifest().metadata.inference_slots,
                    "would_write": false,
                }));
            }
            let slot_owner =
                resolve_plugin_slot_owner(&args.scope, pack.manifest(), &requested).await?;
            let home = args
                .scope
                .home
                .clone()
                .context("asset and plugins packs require --home")?;
            let notification_scope = plugin_notification_scope(&home).await?;
            let notify_scope = notification_scope.clone();
            let notify_home = home.clone();
            let result = gents::pack::plugin_lifecycle::install(
                &home,
                &pack.archive,
                args.grant_authority,
                slot_owner.as_deref(),
                &requested,
                move |identity| {
                    let home = notify_home;
                    async move {
                        if let Some((access, owner)) = notify_scope {
                            gents::pack::record_plugin_store_change(
                                &access,
                                &owner,
                                &home,
                                &identity.coordinate,
                                Some(&identity),
                            )
                            .await
                        } else {
                            Ok(())
                        }
                    }
                },
            )
            .await;
            if let Some((access, _)) = &notification_scope {
                if let Some(node) = access.local_node() {
                    node.shutdown().await;
                }
            }
            let (record, installed_plugins) = result?;
            let root = home.join(&record.assets);
            let mut output = json!({
                "pack": pack.manifest().name,
                "digest": pack.digest(),
                "installed_assets": root,
                "installed_plugins": installed_plugins,
                "record": record,
                "source": pack.label(),
            });
            if !args.wire.is_empty() {
                let (access, owner) = resolve_scope_owner(&args.scope).await?;
                output["wired"] = wire_pack_plugins(&access, &owner, &home, &pack, &args.wire)
                    .await?
                    .into();
            }
            crate::print_json(&output)
        }
    }
}
