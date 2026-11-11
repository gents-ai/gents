use crate::pack::archive::PackArchive;
use crate::pack::resolve::{resolve_named, ResolveOptions, ResolvedFrom};
use crate::pack::PackManifest;
use anyhow::Result;

/// A verified pack archive with the provenance it was resolved from.
pub struct ResolvedPack {
    archive: std::sync::Arc<PackArchive>,
    from: crate::pack::PackSource,
}

impl ResolvedPack {
    /// A verified archive the runtime received from its operator.
    pub fn received(archive: PackArchive, source: crate::pack::PackSource) -> Result<Self> {
        source.validate()?;
        if let crate::pack::PackSource::Registry {
            namespace,
            name,
            version,
            ..
        } = &source
        {
            anyhow::ensure!(
                *namespace == archive.manifest().metadata.namespace
                    && *name == archive.manifest().name
                    && *version == archive.manifest().version,
                "registry source does not match the archive"
            );
        }
        Ok(Self {
            archive: archive.into(),
            from: source,
        })
    }

    pub fn shared_archive(&self) -> std::sync::Arc<PackArchive> {
        self.archive.clone()
    }

    pub fn manifest(&self) -> &PackManifest {
        self.archive.manifest()
    }

    pub fn digest(&self) -> &str {
        self.archive.digest()
    }

    pub fn asset(&self, path: &str) -> Result<&[u8]> {
        self.archive.asset(path)
    }

    pub fn archive(&self) -> &PackArchive {
        &self.archive
    }

    pub fn label(&self) -> &'static str {
        self.from.kind()
    }

    /// Records the provenance of `root` and `dependencies` under their pack coordinates.
    pub fn record_graph_sources(
        prepared: &mut crate::pack::PreparedDocumentPackInstall,
        root: &Self,
        dependencies: &[Self],
    ) {
        for source in std::iter::once(root).chain(dependencies) {
            let manifest = source.manifest();
            prepared.sources.insert(
                format!("{}/{}", manifest.metadata.namespace, manifest.name),
                source.provenance(),
            );
        }
    }

    pub fn provenance(&self) -> crate::pack::PackSource {
        self.from.clone()
    }

    pub fn describe(&self) -> String {
        match &self.from {
            crate::pack::PackSource::Registry {
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

pub async fn resolve_pack_source(
    spec: &str,
    registry_override: Option<&str>,
    home: &std::path::Path,
) -> Result<ResolvedPack> {
    if let Some(local) = crate::pack::local::classify(spec) {
        let archive = crate::pack::local::open(&local, home)?;
        let from = crate::pack::PackSource::of_local_name(&local);
        return Ok(ResolvedPack {
            archive: archive.into(),
            from,
        });
    }
    let installed: Vec<crate::pack::InstalledPack> = crate::pack::list_home_installs(home)?
        .into_iter()
        .map(|record| crate::pack::InstalledPack {
            coordinate: record.coordinate,
            version: record.version,
            digest: record.digest,
        })
        .collect();
    let options = ResolveOptions {
        home: Some(home),
        registry_url: crate::pack::registry::resolve_registry_url(registry_override),
        installed: &installed,
    };
    let resolved = resolve_named(spec, &options).await?;
    let from = match resolved.from {
        ResolvedFrom::Installed => crate::pack::PackSource::Installed,
        ResolvedFrom::Store => crate::pack::PackSource::Store,
        ResolvedFrom::Registry {
            artifact_digest,
            version,
        } => {
            let parsed = gents_documents::pack_spec::parse_pack_spec(spec)?;
            crate::pack::PackSource::Registry {
                namespace: parsed.namespace.to_owned(),
                name: parsed.name.to_owned(),
                version,
                artifact_digest,
            }
        }
    };
    Ok(ResolvedPack {
        archive: resolved.archive.into(),
        from,
    })
}

pub fn materialize_cached_pack(
    home: &std::path::Path,
    pack: &ResolvedPack,
) -> Result<(std::path::PathBuf, gents_store::local::file_lock::FileLock)> {
    crate::pack::materialize::cached(home, &pack.archive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolution_falls_back_to_the_registry_and_reports_the_offline_sentence() {
        let result = resolve_pack_source(
            "definitely_not_a_stored_pack",
            Some("http://127.0.0.1:1"),
            tempfile::tempdir().unwrap().path(),
        )
        .await;
        let Err(error) = result else {
            panic!("neither the store nor the registry has this pack");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("is not in the pack store of")
                && message.contains("could not be reached"),
            "{message}"
        );
        assert!(
            message.contains("definitely_not_a_stored_pack"),
            "{message}"
        );
    }
}
