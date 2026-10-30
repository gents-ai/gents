use super::{local, registry};
use anyhow::{Context, Result};
use gents::pack::archive::PackArchive;
use gents::pack::resolve::{resolve_named, ResolveOptions, ResolvedFrom};
use gents::pack::PackManifest;

pub(crate) struct PackSource {
    pub(super) archive: std::sync::Arc<PackArchive>,
    pub(super) from: Source,
}

pub(super) enum Source {
    Local,
    Installed,
    Store,
    Registry {
        namespace: String,
        name: String,
        version: String,
        artifact_digest: String,
    },
}

impl PackSource {
    /// A verified archive the runtime received from its operator.
    pub(crate) fn received(archive: PackArchive) -> Self {
        Self {
            archive: archive.into(),
            from: Source::Local,
        }
    }

    pub(crate) fn shared_archive(&self) -> std::sync::Arc<PackArchive> {
        self.archive.clone()
    }

    pub(crate) fn manifest(&self) -> &PackManifest {
        self.archive.manifest()
    }

    pub(crate) fn digest(&self) -> &str {
        self.archive.digest()
    }

    pub(crate) fn asset(&self, path: &str) -> Result<&[u8]> {
        self.archive.asset(path)
    }

    pub(crate) fn archive(&self) -> &PackArchive {
        &self.archive
    }

    pub(crate) fn label(&self) -> &'static str {
        match &self.from {
            Source::Local => "local",
            Source::Installed => "installed",
            Source::Store => "store",
            Source::Registry { .. } => "registry",
        }
    }

    pub(crate) fn describe(&self) -> String {
        match &self.from {
            Source::Registry {
                namespace,
                name,
                version,
                artifact_digest,
            } => format!(
                "{} ({namespace}/{name}@{version}, artifact sha256:{artifact_digest})",
                self.label()
            ),
            _ => format!("{} ({})", self.label(), self.digest()),
        }
    }
}

pub(crate) async fn resolve_pack_source(
    spec: &str,
    registry_override: Option<&str>,
    home: &std::path::Path,
) -> Result<PackSource> {
    if let Some(local) = local::classify(spec) {
        let archive = local::open(&local, home)?;
        return Ok(PackSource {
            archive: archive.into(),
            from: Source::Local,
        });
    }
    let installed: Vec<gents::pack::InstalledPack> = gents::pack::list_home_installs(home)?
        .into_iter()
        .map(|record| gents::pack::InstalledPack {
            coordinate: record.coordinate,
            version: record.version,
            digest: record.digest,
        })
        .collect();
    let options = ResolveOptions {
        home: Some(home),
        registry_url: registry::resolve_registry_url(registry_override),
        installed: &installed,
    };
    let resolved = resolve_named(spec, &options).await?;
    let from = match resolved.from {
        ResolvedFrom::Installed => Source::Installed,
        ResolvedFrom::Store => Source::Store,
        ResolvedFrom::Registry {
            artifact_digest,
            version,
        } => {
            let parsed = gents::pack::resolve::parse_pack_spec(spec)?;
            Source::Registry {
                namespace: parsed.namespace.to_owned(),
                name: parsed.name.to_owned(),
                version,
                artifact_digest,
            }
        }
    };
    Ok(PackSource {
        archive: resolved.archive.into(),
        from,
    })
}

pub(super) fn materialize_cached_pack(
    home: &std::path::Path,
    pack: &PackSource,
) -> Result<(std::path::PathBuf, gents::file_lock::FileLock)> {
    gents::pack::materialize::cached(home, &pack.archive)
}

pub(crate) struct SubjectPack {
    pub(crate) source: gents::eval::runner::CellSource,
    pub(crate) manifest: PackManifest,
    _lease: Option<gents::file_lock::FileLock>,
}

impl SubjectPack {
    pub(crate) fn directory(&self) -> &std::path::Path {
        let gents::eval::runner::CellSource::Directory(dir) = &self.source;
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
            source: gents::eval::runner::CellSource::Directory(path.to_path_buf()),
            manifest,
            _lease: None,
        });
    }
    let pack = resolve_pack_source(spec, registry, home).await?;
    let manifest = pack.manifest().clone();
    let (root, lease) = materialize_cached_pack(home, &pack)?;
    Ok(SubjectPack {
        source: gents::eval::runner::CellSource::Directory(root),
        manifest,
        _lease: Some(lease),
    })
}

pub(crate) fn single_slot(manifest: &PackManifest) -> Result<&gents::pack::PackInferenceSlot> {
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
    pack: &PackSource,
) -> Result<std::path::PathBuf> {
    gents::pack::materialize::cache_root(
        home,
        &pack.manifest().metadata.namespace,
        &pack.manifest().name,
        pack.digest(),
    )
}
