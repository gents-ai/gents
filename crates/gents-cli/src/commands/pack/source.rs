use anyhow::{Context, Result};
use gents_pack::pack::resolved_pack::{materialize_cached_pack, resolve_pack_source, ResolvedPack};
use gents_pack::pack::PackManifest;

pub(crate) struct SubjectPack {
    pub(crate) source: gents_eval::runner::CellSource,
    pub(crate) manifest: PackManifest,
    _lease: Option<gents_store::local::file_lock::FileLock>,
}

impl SubjectPack {
    pub(crate) fn directory(&self) -> &std::path::Path {
        let gents_eval::runner::CellSource::Directory(dir) = &self.source;
        dir
    }

    pub(crate) fn default_agent(&self) -> Result<String> {
        let mut agents: Vec<&String> = self
            .manifest
            .metadata
            .inference_slots
            .iter()
            .flat_map(|slot| slot.agents.iter())
            .collect();
        agents.sort();
        agents.dedup();
        match agents.as_slice() {
            [only] => Ok((*only).clone()),
            _ => anyhow::bail!(
                "pack {} declares {} agents in its inference slots; name one as <pack>:<agent>",
                self.manifest.name,
                agents.len()
            ),
        }
    }
}

pub(crate) async fn resolve_subject_pack(
    home: &std::path::Path,
    spec: &str,
    registry: Option<&str>,
) -> Result<SubjectPack> {
    let path = std::path::Path::new(spec);
    if names_a_directory(spec) {
        let manifest_path = path.join("manifest.json");
        let manifest: PackManifest = serde_json::from_slice(
            &std::fs::read(&manifest_path)
                .with_context(|| format!("reading {}", manifest_path.display()))?,
        )
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
        return Ok(SubjectPack {
            source: gents_eval::runner::CellSource::Directory(path.to_path_buf()),
            manifest,
            _lease: None,
        });
    }
    let pack = resolve_pack_source(spec, registry, home).await?;
    let manifest = pack.manifest().clone();
    let (root, lease) = materialize_cached_pack(home, &pack)?;
    Ok(SubjectPack {
        source: gents_eval::runner::CellSource::Directory(root),
        manifest,
        _lease: Some(lease),
    })
}

pub(crate) fn single_slot(manifest: &PackManifest) -> Result<&gents_pack::pack::PackInferenceSlot> {
    match manifest.metadata.inference_slots.as_slice() {
        [only] => Ok(only),
        slots => anyhow::bail!(
            "pack {} declares {} inference slots; expected exactly one",
            manifest.name,
            slots.len()
        ),
    }
}

pub(crate) fn single_slot_agent(manifest: &PackManifest) -> Result<(String, String)> {
    let slot = single_slot(manifest)?;
    match slot.agents.as_slice() {
        [only] => Ok((slot.name.clone(), only.clone())),
        agents => anyhow::bail!(
            "pack {} slot {} declares {} agents; expected exactly one",
            manifest.name,
            slot.name,
            agents.len()
        ),
    }
}

pub(super) fn names_a_directory(spec: &str) -> bool {
    spec.starts_with(['.', '/', '~'])
        || (spec.contains(std::path::is_separator) && std::path::Path::new(spec).is_dir())
}

pub(super) fn asset_cache_root(
    home: &std::path::Path,
    pack: &ResolvedPack,
) -> Result<std::path::PathBuf> {
    gents_pack::pack::materialize::cache_root(
        home,
        &pack.manifest().metadata.namespace,
        &pack.manifest().name,
        pack.digest(),
    )
}
