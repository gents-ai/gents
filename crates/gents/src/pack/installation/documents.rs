use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::config_client::ConfigAccess;
use crate::document_config::PackConfig;
use crate::graph_package::{GraphInstallRecord, GraphPackageInstallBindings, LoadedGraphPackage};
use crate::pack_archive::PackArchive;

use super::super::{PackInferenceBindingPreview, PackInferenceBindings, PackInstallOptions};
use super::{DriftPolicy, InstallReport, InstalledPackPlugin, PackIdentity};

pub struct PreparedDocumentPackInstall {
    pub config: PackConfig,
    pub inference: PackInferenceBindingPreview,
    pub dependency_inference: BTreeMap<String, PackInferenceBindingPreview>,
    pub dependencies: Vec<(LoadedGraphPackage, GraphPackageInstallBindings)>,
}

pub async fn prepare_document_pack_install(
    access: &ConfigAccess,
    owner: &str,
    manifest: &super::super::PackManifest,
    config: &PackConfig,
    requested: &PackInferenceBindings,
    dependencies: &[&PackArchive],
    environment: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Result<PreparedDocumentPackInstall> {
    for dependency in dependencies {
        anyhow::ensure!(
            dependency.manifest().metadata.kind == super::super::PackKind::Graph,
            "only graph dependencies are currently installable"
        );
    }
    for slot in requested.keys() {
        anyhow::ensure!(
            std::iter::once(manifest)
                .chain(dependencies.iter().map(|p| p.manifest()))
                .any(|m| m
                    .metadata
                    .inference_slots
                    .iter()
                    .any(|declared| &declared.name == slot)),
            "pack {} and its dependencies have no inference slot {slot:?}",
            manifest.name
        );
    }
    let selected = |manifest: &super::super::PackManifest| {
        requested
            .iter()
            .filter(|(slot, _)| {
                manifest
                    .metadata
                    .inference_slots
                    .iter()
                    .any(|declared| &declared.name == *slot)
            })
            .map(|(slot, profile)| (slot.clone(), profile.clone()))
            .collect()
    };
    let inference =
        super::super::preview_pack_inference_bindings(access, manifest, owner, &selected(manifest))
            .await?;
    let config = super::super::bind_pack_install_config(manifest, config, &inference.bindings)?;
    let mut dependency_inference = BTreeMap::new();
    let mut prepared_dependencies = Vec::new();
    for archive in dependencies {
        let preview = super::super::preview_pack_inference_bindings(
            access,
            archive.manifest(),
            owner,
            &selected(archive.manifest()),
        )
        .await?;
        let package = crate::graph_package::load_archive_graph_package_with_environment(
            archive,
            &PackInstallOptions {
                agent_did: owner.to_owned(),
            },
            environment,
        )?;
        let bindings = GraphPackageInstallBindings {
            agent_did: owner.to_owned(),
            inference_slots: preview.bindings.clone(),
        };
        crate::graph_package::prepare_loaded_graph_package_install(access, &package, &bindings)
            .await?;
        dependency_inference.insert(archive.manifest().name.clone(), preview);
        prepared_dependencies.push((package, bindings));
    }
    Ok(PreparedDocumentPackInstall {
        config,
        inference,
        dependency_inference,
        dependencies: prepared_dependencies,
    })
}

pub async fn install_prepared_document_pack(
    access: &ConfigAccess,
    owner: &str,
    archive: &PackArchive,
    prepared: &PreparedDocumentPackInstall,
    plugin_home: Option<&Path>,
    grant_authority: bool,
    policy: DriftPolicy,
) -> Result<InstallReport> {
    let all = std::iter::once(archive.manifest())
        .chain(prepared.dependencies.iter().map(|(p, _)| &p.manifest));
    for manifest in all {
        for plugin in &manifest.metadata.plugins {
            let home = plugin_home.context("pack plugins require the node's local plugin home")?;
            crate::plugin::store::grant_on_install(
                home,
                &manifest.metadata.namespace,
                plugin,
                grant_authority,
            )?;
        }
    }
    let mut schemas = Vec::new();
    for path in &archive.manifest().schemas {
        let sdl = std::str::from_utf8(archive.asset(path)?)?;
        schemas.push((
            path,
            crate::config_client::preview_schema_install(access, sdl).await?,
        ));
    }
    for (package, bindings) in &prepared.dependencies {
        let (plugins, rollback) = install_plugins(
            owner,
            plugin_home,
            &package.manifest,
            &package.package_digest,
            &bindings.inference_slots,
            &|path| package.asset(path),
            grant_authority,
        )?;
        let record = GraphInstallRecord {
            plugins,
            explicit: false,
        };
        let installed = crate::graph_package::install_loaded_graph_package(
            access, owner, package, bindings, None, &record,
        )
        .await;
        let receipt = installed.inspect_err(|_| {
            if let Some(home) = plugin_home {
                crate::plugin::install::rollback_pack_plugin_records(home, &rollback);
            }
        })?;
        crate::graph_pipeline::activate_graph_revision_with_access(
            access,
            owner,
            &receipt.graph_id,
            &receipt.revision_digest,
            receipt.predecessor_revision_digest.as_deref(),
        )
        .await?;
    }
    for (path, plan) in schemas {
        crate::config_client::apply_schema_install(
            access,
            std::str::from_utf8(archive.asset(path)?)?,
            &plan.artifact_digest,
        )
        .await?;
    }
    let (plugins, rollback) = install_plugins(
        owner,
        plugin_home,
        archive.manifest(),
        archive.digest(),
        &prepared.inference.bindings,
        &|path| archive.asset(path),
        grant_authority,
    )?;
    let mut identity = PackIdentity::new(archive.manifest(), archive.digest(), plugins);
    identity.dependencies = prepared
        .dependencies
        .iter()
        .map(|(p, _)| format!("{}/{}", p.manifest.metadata.namespace, p.manifest.name))
        .collect();
    super::super::install_pack_documents(access, owner, &identity, &prepared.config, policy)
        .await
        .inspect_err(|_| {
            if let Some(home) = plugin_home {
                crate::plugin::install::rollback_pack_plugin_records(home, &rollback);
            }
        })
}

type PluginRollback = Vec<(
    String,
    String,
    Option<crate::plugin::store::InstalledPlugin>,
)>;

fn install_plugins<'a>(
    owner: &str,
    home: Option<&Path>,
    manifest: &super::super::PackManifest,
    digest: &str,
    bindings: &PackInferenceBindings,
    asset: &dyn Fn(&str) -> Result<&'a [u8]>,
    grant_authority: bool,
) -> Result<(Vec<InstalledPackPlugin>, PluginRollback)> {
    if manifest.metadata.plugins.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let home = home.context("pack plugins require the node's local plugin home")?;
    let rollback = crate::plugin::install::snapshot_pack_plugin_records(home, manifest);
    let plugins = crate::plugin::install::install_pack_plugins(
        home,
        manifest,
        digest,
        asset,
        grant_authority,
    )
    .inspect_err(|_| crate::plugin::install::rollback_pack_plugin_records(home, &rollback))?;
    crate::plugin::install::bind_plugin_slots(home, manifest, owner, bindings)
        .inspect_err(|_| crate::plugin::install::rollback_pack_plugin_records(home, &rollback))?;
    Ok((
        plugins
            .iter()
            .map(|p| InstalledPackPlugin {
                name: p.name.clone(),
                digest: p.digest.clone(),
            })
            .collect(),
        rollback,
    ))
}
