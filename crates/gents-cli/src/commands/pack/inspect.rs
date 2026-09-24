//! `gents pack show` and `gents pack verify`: looking at a pack without
//! installing it.

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::{PackShowArgs, PackVerifyArgs};

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
