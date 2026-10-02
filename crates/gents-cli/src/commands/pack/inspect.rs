//! `gents pack show` and `gents pack verify`: looking at a pack without
//! installing it.

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::{PackDiffArgs, PackShowArgs, PackVerifyArgs};

/// The owner a `--config` preview is bound to. Never written anywhere.
const SHOW_CONFIG_OWNER: &str = "did:key:zPackShowPlaceholder";

/// `gents pack show`: the manifest, the pack digest and every file with its
/// size and sha256, for any pack `install` accepts; with `--config`, instead
/// prints `{config, scenario}` as an install would load them, at their
/// declared `${VAR:-default}` defaults (the environment is never read).
pub(super) async fn show(args: PackShowArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let pack = super::resolve_pack_source(&args.package, args.registry.as_deref(), &home).await?;
    if args.config {
        return show_config(&pack);
    }
    let manifest = pack.manifest();
    let dependency_origins = manifest
        .metadata
        .dependencies
        .iter()
        .map(|dependency| {
            let (_, name) = super::split_namespace(dependency);
            Ok(json!({
                "pack": dependency,
                "origin_tag": gents::pack::pack_origin_tag(name)?,
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    let files = gents::pack::declared_paths(manifest)
        .into_iter()
        .map(|path| {
            use sha2::Digest;
            let bytes = pack.asset(&path)?;
            Ok(json!({
                "path": path,
                "size_bytes": bytes.len(),
                "sha256": format!("{:x}", sha2::Sha256::digest(bytes)),
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    crate::print_json(&json!({
        "origin_tag": gents::pack::pack_origin_tag(&manifest.name)?,
        "source": pack.describe(),
        "dependency_origins": dependency_origins,
        "manifest": manifest,
        "digest": pack.digest(),
        "files": files,
    }))
}

/// `gents pack show --config`'s `{config, scenario}`: the pack's canonical
/// configuration, loaded exactly like an install would (for a placeholder
/// owner, since nothing is written), and its `experiment.json`, interpolated
/// at defaults, or `null` when the pack declares no scenario. The
/// environment is never read: a missing default is refused so the preview
/// never differs from the pack's own declared defaults.
fn show_config(pack: &super::PackSource) -> Result<()> {
    let manifest = pack.manifest();
    let config = gents::pack::load_pack_config(
        manifest,
        &gents::pack::PackInstallOptions {
            agent_did: SHOW_CONFIG_OWNER.into(),
        },
        &|path| pack.asset(path).map(Vec::from),
        &|_| None,
    )?;
    let scenario = gents::pack::declared_paths(manifest)
        .iter()
        .any(|path| path == "experiment.json")
        .then(|| -> Result<serde_json::Value> {
            let raw = std::str::from_utf8(pack.asset("experiment.json")?)
                .context("experiment.json is not UTF-8")?;
            let expanded = crate::desired_state::interpolate::interpolate_with(raw, &|_| None)
                .map_err(|missing| {
                    anyhow::anyhow!(
                        "experiment.json references environment variable(s) with no default: {}",
                        missing.join(", ")
                    )
                })?;
            serde_json::from_str(&expanded).context("parsing experiment.json")
        })
        .transpose()?;
    crate::print_json(&json!({ "config": config, "scenario": scenario }))
}

/// `gents pack verify`: checks a `.pack` file, or a pack in the home's
/// store, streaming, without installing or storing anything.
pub(super) fn verify(args: PackVerifyArgs) -> Result<()> {
    let header = if args.target.starts_with("sha256:") {
        let home = crate::home_state::resolve_home_dir(args.home.as_deref());
        gents::pack_store::PackStore::new(&home).verify(&args.target)?
    } else {
        let path = std::path::Path::new(&args.target);
        let file =
            std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        gents::pack_archive::read_pack(
            std::io::BufReader::new(file),
            gents::pack_archive::Bounds::default(),
            |_, _| Ok(()),
        )
        .with_context(|| format!("{} is not a valid pack", path.display()))?
        .header
    };
    crate::print_json(&json!({ "verified": true, "pack": header }))
}

/// The owner a compared pack's documents are bound to. Never written.
const COMPARED_OWNER: &str = "did:key:zPackDiffPlaceholderOwner";

/// `gents pack diff`: the files and configuration documents that differ
/// between two packs, each named as `install` accepts.
pub(super) async fn diff(args: PackDiffArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let a = super::resolve_pack_source(&args.a, args.registry.as_deref(), &home).await?;
    let b = super::resolve_pack_source(&args.b, args.registry.as_deref(), &home).await?;
    crate::print_json(&pack_diff(&a, &b)?)
}

pub(super) fn pack_diff(a: &super::PackSource, b: &super::PackSource) -> Result<serde_json::Value> {
    let (files_a, files_b) = (file_digests(a)?, file_digests(b)?);
    let (docs_a, docs_b) = (document_digests(a)?, document_digests(b)?);
    Ok(json!({
        "a": {"pack": a.manifest().name, "version": a.manifest().version, "digest": a.digest()},
        "b": {"pack": b.manifest().name, "version": b.manifest().version, "digest": b.digest()},
        "files": changes(&files_a, &files_b),
        "documents": changes(&docs_a, &docs_b),
    }))
}

fn file_digests(pack: &super::PackSource) -> Result<std::collections::BTreeMap<String, String>> {
    use sha2::Digest;
    gents::pack::declared_paths(pack.manifest())
        .into_iter()
        .map(|path| {
            let digest = format!("{:x}", sha2::Sha256::digest(pack.asset(&path)?));
            Ok((path, digest))
        })
        .collect()
}

fn document_digests(
    pack: &super::PackSource,
) -> Result<std::collections::BTreeMap<String, String>> {
    if pack.manifest().config.is_none() {
        return Ok(Default::default());
    }
    let config = gents::pack::load_pack_config(
        pack.manifest(),
        &gents::pack::PackInstallOptions {
            agent_did: COMPARED_OWNER.into(),
        },
        &|path| pack.asset(path).map(Vec::from),
        &|_| None,
    )?;
    Ok(gents::pack::pack_document_digests(&config)?
        .into_iter()
        .map(|((collection, id), digest)| (format!("{collection}/{id}"), digest))
        .collect())
}

fn changes(
    a: &std::collections::BTreeMap<String, String>,
    b: &std::collections::BTreeMap<String, String>,
) -> serde_json::Value {
    let added: Vec<&String> = b.keys().filter(|key| !a.contains_key(*key)).collect();
    let removed: Vec<&String> = a.keys().filter(|key| !b.contains_key(*key)).collect();
    let changed: Vec<&String> = a
        .iter()
        .filter(|(key, digest)| b.get(*key).is_some_and(|other| other != *digest))
        .map(|(key, _)| key)
        .collect();
    json!({"added": added, "removed": removed, "changed": changed})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_pack_and_its_edited_copy_differ_by_exactly_the_edit() {
        let home = tempfile::tempdir().unwrap();
        let original =
            super::super::test_support::fixture_pack_source("documents_fixture", home.path());
        let dir = tempfile::tempdir().unwrap();
        for path in gents::pack::declared_paths(original.manifest()) {
            let target = dir.path().join(&path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, original.asset(&path).unwrap()).unwrap();
        }
        let prompt = dir.path().join("tasks/fixture_worker_task/prompt.md");
        std::fs::write(&prompt, "a different prompt").unwrap();
        let edited = super::super::test_support::local_pack_source(dir.path(), home.path());

        let report = pack_diff(&original, &edited).unwrap();
        assert_eq!(
            report["files"]["changed"],
            json!(["tasks/fixture_worker_task/prompt.md"])
        );
        assert_eq!(report["files"]["added"], json!([]));
        assert_eq!(
            report["documents"]["changed"],
            json!(["Task/fixture-worker-task"])
        );
        assert_eq!(report["documents"]["removed"], json!([]));
    }

    #[tokio::test]
    async fn show_config_prints_the_loaded_configuration_and_scenario() {
        let home = tempfile::tempdir().unwrap();
        let fixture_dir = super::super::test_support::fixture_dir("documents_fixture");
        let report = crate::request_helpers::capture_report(show(PackShowArgs {
            package: fixture_dir.to_str().unwrap().to_owned(),
            home: Some(home.path().to_path_buf()),
            registry: Some("http://127.0.0.1:1".to_owned()),
            config: true,
        }))
        .await
        .unwrap();
        assert_eq!(
            report["config"]["agent_behaviors"][0]["behavior_id"],
            "fixture-worker"
        );
        assert_eq!(report["scenario"]["seed"]["collection"], "FixtureJob");
    }
}
