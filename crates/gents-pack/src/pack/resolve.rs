use std::path::Path;

use anyhow::Result;

use crate::pack::archive::PackArchive;
use crate::pack::registry::{fetch_pack, RegistryClient, RegistryError};
use crate::pack::store::PackStore;
use crate::pack::InstalledPack;
use gents_documents::pack_spec::parse_pack_spec;

#[derive(Debug, Clone)]
pub enum ResolvedFrom {
    Installed,
    Store,
    Registry {
        artifact_digest: String,
        version: String,
    },
}

pub struct ResolvedNamedPack {
    pub archive: PackArchive,
    pub from: ResolvedFrom,
}

impl ResolvedNamedPack {
    /// A pack opened by path or digest: opening admits it to the home's
    /// store, so that is where it resolves from.
    pub fn stored(archive: PackArchive) -> Self {
        Self {
            archive,
            from: ResolvedFrom::Store,
        }
    }
}

pub struct ResolveOptions<'a> {
    pub home: Option<&'a Path>,
    pub registry_url: String,
    pub installed: &'a [InstalledPack],
}

pub async fn resolve_named(spec: &str, options: &ResolveOptions<'_>) -> Result<ResolvedNamedPack> {
    let parsed = parse_pack_spec(spec)?;
    let coordinate = format!("{}/{}", parsed.namespace, parsed.name);

    if let Some(home) = options.home {
        let store = PackStore::new(home);
        for installed in options.installed.iter().filter(|record| {
            record.coordinate == coordinate
                && parsed
                    .version
                    .is_none_or(|version| record.version == version)
        }) {
            if store.contains(&installed.digest)? {
                return Ok(ResolvedNamedPack {
                    archive: store.open(&installed.digest)?,
                    from: ResolvedFrom::Installed,
                });
            }
        }
        if let Some(found) = store.lookup(parsed.namespace, parsed.name, parsed.version)? {
            return Ok(ResolvedNamedPack {
                archive: store.open(&found.digest)?,
                from: ResolvedFrom::Store,
            });
        }
    }

    let client = RegistryClient::new(options.registry_url.clone());
    let fetched = fetch_pack(
        &client,
        options.home,
        parsed.namespace,
        parsed.name,
        parsed.version,
    )
    .await
    .map_err(|error| {
        offline_pack_error(
            &coordinate,
            parsed.version,
            &options.registry_url,
            options.home,
            error,
        )
    })?;
    Ok(ResolvedNamedPack {
        archive: fetched.archive,
        from: ResolvedFrom::Registry {
            artifact_digest: fetched.artifact_digest,
            version: fetched.version,
        },
    })
}

fn offline_pack_error(
    coordinate: &str,
    version: Option<&str>,
    registry_url: &str,
    home: Option<&Path>,
    error: anyhow::Error,
) -> anyhow::Error {
    let Some(home) = home else {
        return error.context(format!("resolving {coordinate} from the registry"));
    };
    let pinned = match version {
        Some(version) => format!("{coordinate}@{version}"),
        None => coordinate.to_owned(),
    };
    match RegistryError::in_chain(&error) {
        Some(RegistryError::Unreachable { .. }) => anyhow::anyhow!(
            "{pinned} is not in the pack store of {} and the registry at {registry_url} \
             could not be reached; fetch it while online with `gents config pack fetch {pinned} \
             --store`, or name a directory or .pack file.",
            home.display()
        ),
        Some(RegistryError::NotFound { .. }) => anyhow::anyhow!(
            "{pinned} is not in the pack store of {} and the registry at {registry_url} \
             has no such pack.",
            home.display()
        ),
        None => error.context(format!("resolving {coordinate} from the registry")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::store::test_pack_named as build_pack;

    const UNROUTABLE_REGISTRY: &str = "http://127.0.0.1:1";

    fn expect_err(result: Result<ResolvedNamedPack>) -> anyhow::Error {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Origin {
        Installed,
        Store,
        Registry,
    }

    #[tokio::test]
    async fn resolution_conforms_to_the_model() {
        use gents_lean_vocab::families::pack_resolution as family;

        for case in family::cases::<Origin>() {
            let input = &case.input;
            let home = tempfile::tempdir().unwrap();
            let store = PackStore::new(home.path());
            for pack in &input.received {
                let (bytes, _) = build_pack(&pack.name, &pack.version.text);
                store.import(bytes.as_slice(), None).unwrap();
            }
            for pack in &input.lost {
                let (_, header) = build_pack(&pack.name, &pack.version.text);
                std::fs::remove_file(store.path(&header.digest).unwrap()).unwrap();
            }
            let installed: Vec<InstalledPack> = input
                .installed
                .iter()
                .map(|pack| {
                    let (_, header) = build_pack(&pack.name, &pack.version.text);
                    InstalledPack {
                        coordinate: format!("gents/{}", pack.name),
                        version: pack.version.text.clone(),
                        digest: header.digest,
                        source: None,
                    }
                })
                .collect();
            let options = ResolveOptions {
                home: Some(home.path()),
                registry_url: UNROUTABLE_REGISTRY.to_owned(),
                installed: &installed,
            };
            let spec = match &input.version {
                Some(version) => format!("gents/{}@{version}", input.ask),
                None => format!("gents/{}", input.ask),
            };
            let resolved = match resolve_named(&spec, &options).await {
                Ok(resolved) => family::Resolution {
                    origin: match resolved.from {
                        ResolvedFrom::Installed => Origin::Installed,
                        ResolvedFrom::Store => Origin::Store,
                        ResolvedFrom::Registry { .. } => Origin::Registry,
                    },
                    version: Some(resolved.archive.header().version.clone()),
                },
                Err(_) => family::Resolution {
                    origin: Origin::Registry,
                    version: None,
                },
            };
            case.check(resolved);
        }
    }

    #[tokio::test]
    async fn an_unreachable_registry_fails_with_the_offline_sentence() {
        let home = tempfile::tempdir().unwrap();
        let options = ResolveOptions {
            home: Some(home.path()),
            registry_url: UNROUTABLE_REGISTRY.to_owned(),
            installed: &[],
        };
        let error = expect_err(resolve_named("gents/nowhere", &options).await);
        let message = format!("{error:#}");
        assert!(
            message.contains("is not in the pack store of")
                && message.contains("could not be reached"),
            "{message}"
        );
        assert!(
            message.contains("gents config pack fetch gents/nowhere --store"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_registry_keeps_the_pin_in_the_offline_sentence() {
        let home = tempfile::tempdir().unwrap();
        let options = ResolveOptions {
            home: Some(home.path()),
            registry_url: UNROUTABLE_REGISTRY.to_owned(),
            installed: &[],
        };
        let error = expect_err(resolve_named("gents/nowhere@9.9.9", &options).await);
        let message = format!("{error:#}");
        assert!(message.contains("gents/nowhere@9.9.9"), "{message}");
        assert!(
            message.contains("gents config pack fetch gents/nowhere@9.9.9 --store"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_registry_that_has_never_heard_of_the_pack_fails_with_the_404_sentence() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, axum::Router::new()).await;
        });

        let home = tempfile::tempdir().unwrap();
        let options = ResolveOptions {
            home: Some(home.path()),
            registry_url: format!("http://{addr}"),
            installed: &[],
        };
        let error = expect_err(resolve_named("gents/nowhere", &options).await);
        let message = format!("{error:#}");
        assert!(
            message.contains("is not in the pack store of") && message.contains("has no such pack"),
            "{message}"
        );
    }
}
