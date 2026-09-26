//! `gents pack show` and `gents pack verify`: looking at a pack without
//! installing it.

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::{PackDiffArgs, PackShowArgs, PackVerifyArgs};

/// `gents pack show`: the manifest, the pack digest and every file with its
/// size and sha256, for any pack `install` accepts.
pub(super) async fn show(args: PackShowArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let pack = super::resolve_pack_source(&args.package, args.registry.as_deref(), &home).await?;
    let manifest = pack.manifest();
    let dependency_origins = manifest
        .metadata
        .dependencies
        .iter()
        .map(|dependency| {
            Ok(json!({
                "pack": dependency,
                "origin_tag": gents::pack::pack_origin_tag(dependency)?,
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
        let bundled = gents::pack::resolve_pack("lsp_rust").unwrap();
        let dir = tempfile::tempdir().unwrap();
        for path in gents::pack::declared_paths(&bundled.manifest) {
            let target = dir.path().join(&path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, bundled.asset(&path).unwrap()).unwrap();
        }
        let prompt = dir.path().join("tasks/lsp_hover_task/prompt.md");
        std::fs::write(&prompt, "a different prompt").unwrap();
        let home = tempfile::tempdir().unwrap();
        let edited = super::super::local::open(
            &super::super::local::LocalName::Path(dir.path()),
            home.path(),
        )
        .unwrap();

        let report = pack_diff(
            &super::super::PackSource::Bundled(bundled),
            &super::super::PackSource::Stored(edited),
        )
        .unwrap();
        assert_eq!(
            report["files"]["changed"],
            json!(["tasks/lsp_hover_task/prompt.md"])
        );
        assert_eq!(report["files"]["added"], json!([]));
        assert_eq!(
            report["documents"]["changed"],
            json!(["Task/lsp-hover-task"])
        );
        assert_eq!(report["documents"]["removed"], json!([]));
    }
}
