use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::pack::{HomePackInstall, InstalledPackPlugin, RemoveReport};
use serde_json::{json, Value};

use crate::cli::PackRemoveArgs;

pub(crate) async fn remove(args: PackRemoveArgs) -> Result<()> {
    let (namespace, name) = super::split_namespace(&args.package);
    let coordinate = format!("{namespace}/{name}");
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());

    if args.scope.graphql.is_none() {
        if let Some(record) = gents::pack::read_home_install(&home, &coordinate)? {
            anyhow::ensure!(
                args.scope.node_did.is_none(),
                "asset and plugins packs are removed locally with --home; identity flags do not apply"
            );
            let removed = remove_home_install(&home, &coordinate, record).await?;
            return crate::print_json(&json!({ "pack": coordinate, "removed": removed }));
        }
        anyhow::ensure!(
            gents::home::init_config_path(&home).is_file(),
            "{coordinate} is not installed in {}",
            home.display()
        );
    }

    let (access, owner) = super::resolve_scope_owner(&args.scope).await?;
    let report =
        gents::pack::remove_pack(&access, &owner, &coordinate, args.drift.policy()).await?;

    let mut plugin_digests = BTreeSet::new();
    let pack_plugin_records = release_plugin_records(
        &home,
        &report.pack,
        &report.documents.plugins,
        &mut plugin_digests,
    )?;
    let mut dependency_plugin_records = Vec::with_capacity(report.dependencies.len());
    for dependency in &report.dependencies {
        dependency_plugin_records.push(release_plugin_records(
            &home,
            &dependency.pack,
            &dependency.documents.plugins,
            &mut plugin_digests,
        )?);
    }
    let plugin_bytes = plugin_release_result(&home, &plugin_digests)?;

    let mut archive_digests: BTreeSet<String> = report.digests.iter().cloned().collect();
    for dependency in &report.dependencies {
        archive_digests.extend(dependency.digests.iter().cloned());
    }
    let archives = if args.scope.graphql.is_none() {
        release_archives(&home, Some(&access), &archive_digests).await?
    } else {
        Vec::new()
    };

    let removed = augment_report(
        &report,
        &pack_plugin_records,
        &dependency_plugin_records,
        &plugin_bytes,
        &archives,
    )?;
    crate::print_json(&json!({ "pack": coordinate, "owner": owner, "removed": removed }))
}

fn augment_report(
    report: &RemoveReport,
    pack_plugin_records: &[InstalledPackPlugin],
    dependency_plugin_records: &[Vec<InstalledPackPlugin>],
    plugin_bytes: &[String],
    archives: &[String],
) -> Result<Value> {
    let mut value = serde_json::to_value(report).context("encoding the remove report")?;
    let object = value
        .as_object_mut()
        .context("remove report must serialize as an object")?;
    object.insert("plugins".to_owned(), json!(pack_plugin_records));
    if let Some(dependencies) = object.get_mut("dependencies").and_then(Value::as_array_mut) {
        anyhow::ensure!(
            dependencies.len() == dependency_plugin_records.len(),
            "remove report listed {} dependencies but {} were released",
            dependencies.len(),
            dependency_plugin_records.len()
        );
        for (dependency, plugins) in dependencies.iter_mut().zip(dependency_plugin_records) {
            let dependency = dependency
                .as_object_mut()
                .context("dependency report must serialize as an object")?;
            dependency.insert("plugins".to_owned(), json!(plugins));
        }
    }
    object.insert("plugin_bytes".to_owned(), json!(plugin_bytes));
    object.insert("archives".to_owned(), json!(archives));
    Ok(value)
}

fn release_plugin_records(
    home: &Path,
    coordinate: &str,
    plugins: &[InstalledPackPlugin],
    candidates: &mut BTreeSet<String>,
) -> Result<Vec<InstalledPackPlugin>> {
    let (namespace, _) = super::split_namespace(coordinate);
    let mut released = Vec::new();
    for plugin in plugins {
        candidates.insert(plugin.digest.clone());
        if gents::plugin::store::owns_plugin_record(
            home,
            namespace,
            &plugin.name,
            coordinate,
            &plugin.digest,
        ) {
            gents::plugin::store::remove_record(home, namespace, &plugin.name)?;
            released.push(plugin.clone());
        }
    }
    Ok(released)
}

fn plugin_release_result(home: &Path, candidates: &BTreeSet<String>) -> Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    gents::plugin::store::release_unreferenced_bytes(home, candidates)
}

async fn release_archives(
    home: &Path,
    node: Option<&ConfigAccess>,
    digests: &BTreeSet<String>,
) -> Result<Vec<String>> {
    if digests.is_empty() {
        return Ok(Vec::new());
    }
    let referenced = gents::pack::referenced_pack_digests(home, node).await?;
    let store = gents::pack_store::PackStore::new(home);
    let mut released = Vec::new();
    for digest in digests {
        if referenced.contains(digest) {
            continue;
        }
        if store.release(digest)? {
            released.push(digest.clone());
        }
    }
    released.sort();
    Ok(released)
}

async fn remove_home_install(
    home: &Path,
    coordinate: &str,
    record: HomePackInstall,
) -> Result<Value> {
    let mut retained = Vec::new();
    let mut assets_removed = Vec::new();
    let release = super::cache::release_recorded_cache(home, coordinate, &record)?;
    match release {
        super::CacheRelease::Removed => assets_removed.push(record.assets.clone()),
        super::CacheRelease::Retained(reason) => retained.push(json!({
            "item": record.assets,
            "reason": reason,
        })),
    }

    let mut plugin_digests = BTreeSet::new();
    let plugin_records =
        release_plugin_records(home, coordinate, &record.plugins, &mut plugin_digests)?;
    let plugin_bytes = plugin_release_result(home, &plugin_digests)?;
    if !record.plugins.is_empty() {
        super::warn_on_unrecorded_removal(
            coordinate,
            super::record_plugin_store_change(home, coordinate, None).await,
        );
    }

    gents::pack::forget_home_install(home, coordinate)?;
    let mut archive_digests = BTreeSet::new();
    archive_digests.insert(record.digest.clone());
    let archives = release_archives(home, None, &archive_digests).await?;

    Ok(json!({
        "assets": assets_removed,
        "plugins": plugin_records,
        "retained": retained,
        "plugin_bytes": plugin_bytes,
        "archives": archives,
    }))
}

#[cfg(test)]
mod tests;
