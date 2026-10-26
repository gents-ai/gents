use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::pack::{InstalledPackPlugin, PackKind, RemoveReport};
use serde_json::{json, Value};

use crate::cli::PackRemoveArgs;

pub(crate) async fn remove(args: PackRemoveArgs) -> Result<()> {
    let (namespace, name) = super::split_namespace(&args.package);
    let coordinate = format!("{namespace}/{name}");
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());

    // A documents pack's record has no assets or node-free removal: it goes
    // through `remove_pack` below and the record is forgotten after.
    let mut documents_record = false;
    if args.scope.graphql.is_none() {
        if let Some(record) = gents::pack::read_home_install(&home, &coordinate)? {
            if matches!(record.kind, PackKind::Assets | PackKind::Plugins) {
                anyhow::ensure!(
                    args.scope.node_did.is_none(),
                    "asset and plugins packs are removed locally with --home; identity flags do not apply"
                );
                let removed = remove_home_install(&home, &coordinate).await?;
                return Ok(crate::print_json(
                    &json!({ "pack": coordinate, "removed": removed }),
                )?);
            }
            documents_record = true;
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
    if documents_record {
        // Before the archive scan below, which counts every file record.
        gents::pack::forget_home_install(&home, &coordinate)?;
    }

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
    Ok(crate::print_json(
        &json!({ "pack": coordinate, "owner": owner, "removed": removed }),
    )?)
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
    candidates.extend(plugins.iter().map(|plugin| plugin.digest.clone()));
    Ok(gents::pack::plugin_lifecycle::release_plugin_records(
        home, coordinate, plugins,
    )?)
}

fn plugin_release_result(home: &Path, candidates: &BTreeSet<String>) -> Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    Ok(gents::plugin::store::release_unreferenced_bytes(
        home, candidates,
    )?)
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

pub(crate) async fn remove_home_install(home: &Path, coordinate: &str) -> Result<Value> {
    let mut retained = Vec::new();
    let mut assets_removed = Vec::new();
    let removal = gents::pack::plugin_lifecycle::remove_local(
        home,
        coordinate,
        |record| {
            Ok::<_, anyhow::Error>((
                super::cache::prepare_recorded_cache(home, coordinate, record)?,
                record.assets.clone(),
            ))
        },
        |(prepared, recorded_assets)| {
            let release = prepared.finish()?;
            match release {
                super::CacheRelease::Removed => assets_removed.push(recorded_assets.clone()),
                super::CacheRelease::Retained(reason) => retained.push(json!({
                    "item": recorded_assets,
                    "reason": reason,
                })),
            }
            Ok::<_, anyhow::Error>(())
        },
    )?;
    let record = removal.record;
    let plugin_records = removal.plugins;
    if let Some(reason) = removal.cache_error {
        retained.push(json!({"item": record.assets, "reason": reason}));
    }
    let plugin_digests = record
        .plugins
        .iter()
        .map(|plugin| plugin.digest.clone())
        .collect();
    if !record.plugins.is_empty() {
        super::warn_on_unrecorded_removal(
            coordinate,
            super::record_plugin_store_change(home, coordinate, None).await,
        );
    }

    let plugin_bytes = plugin_release_result(home, &plugin_digests).context(
        "the local pack was removed, but unreferenced plugin-byte cleanup did not finish",
    )?;
    let mut archive_digests = BTreeSet::new();
    archive_digests.insert(record.digest.clone());
    let archives = release_archives(home, None, &archive_digests)
        .await
        .context("the local pack was removed, but unreferenced archive cleanup did not finish")?;

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
