use std::path::Path;

use anyhow::Result;

use crate::pack::{is_valid_pack_name, InstalledPack};
use crate::pack_archive::{PackArchive, DEFAULT_NAMESPACE};
use crate::pack_registry::{fetch_pack, RegistryClient, RegistryError};
use crate::pack_store::{is_valid_index_version, PackStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackSpec<'a> {
    pub namespace: &'a str,
    pub name: &'a str,
    pub version: Option<&'a str>,
}

pub fn parse_pack_spec(spec: &str) -> Result<PackSpec<'_>> {
    let (coordinate, version) = match spec.split_once('@') {
        Some((coordinate, version)) => {
            anyhow::ensure!(!version.is_empty(), "{spec:?} names no version after @");
            anyhow::ensure!(
                is_valid_index_version(version),
                "{spec:?} names an invalid version; a version is ASCII letters, \
                 digits, '.', '+' or '-', and never a bare '.' or '..'"
            );
            (coordinate, Some(version))
        }
        None => (spec, None),
    };
    let (namespace, name) = coordinate
        .split_once('/')
        .unwrap_or((DEFAULT_NAMESPACE, coordinate));
    anyhow::ensure!(
        is_valid_pack_name(namespace) && is_valid_pack_name(name),
        "{spec:?} is not a pack name; expected name, ns/name, or either with @version, \
         namespace and name both snake_case"
    );
    Ok(PackSpec {
        namespace,
        name,
        version,
    })
}

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
    match error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RegistryError>())
    {
        Some(RegistryError::Unreachable { .. }) => anyhow::anyhow!(
            "{pinned} is not in the pack store of {} and the registry at {registry_url} \
             could not be reached; fetch it while online with `gents pack fetch {pinned} \
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
    use crate::pack_store::test_pack_named as build_pack;
    use gents_protocol::error_chain::ErrorChain;

    const UNROUTABLE_REGISTRY: &str = "http://127.0.0.1:1";

    fn expect_err(result: Result<ResolvedNamedPack>) -> anyhow::Error {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }

    #[test]
    fn parse_pack_spec_accepts_an_uppercase_semver_pin() {
        assert_eq!(
            parse_pack_spec("demo@1.0.0-RC1").unwrap().version,
            Some("1.0.0-RC1")
        );
    }

    #[test]
    fn parse_pack_spec_refuses_malformed_or_non_snake_case_specs() {
        for spec in [
            "",
            "acme/",
            "/demo",
            "acme/demo/extra",
            "demo@",
            "Acme/Demo",
            "acme/de-mo",
            "demo@1@2",
            "demo@a b",
            "demo@../x",
        ] {
            assert!(parse_pack_spec(spec).is_err(), "{spec:?} should be refused");
        }
    }

    #[tokio::test]
    async fn a_store_hit_needs_no_registry() {
        let home = tempfile::tempdir().unwrap();
        let store = PackStore::new(home.path());
        let (bytes, header) = build_pack("resolve_fixture_a", "1.0.0");
        store.import(bytes.as_slice(), None).unwrap();

        let options = ResolveOptions {
            home: Some(home.path()),
            registry_url: UNROUTABLE_REGISTRY.to_owned(),
            installed: &[],
        };
        let resolved = resolve_named("gents/resolve_fixture_a", &options)
            .await
            .expect("resolved from the store, without touching the network");
        assert!(matches!(resolved.from, ResolvedFrom::Store));
        assert_eq!(resolved.archive.digest(), header.digest);
    }

    #[tokio::test]
    async fn an_at_version_pin_resolves_exactly_that_version() {
        let home = tempfile::tempdir().unwrap();
        let store = PackStore::new(home.path());
        let (v1_bytes, v1) = build_pack("resolve_fixture_b", "1.0.0");
        let (v2_bytes, v2) = build_pack("resolve_fixture_b", "2.0.0");
        store.import(v1_bytes.as_slice(), None).unwrap();
        store.import(v2_bytes.as_slice(), None).unwrap();

        let options = ResolveOptions {
            home: Some(home.path()),
            registry_url: UNROUTABLE_REGISTRY.to_owned(),
            installed: &[],
        };
        let resolved = resolve_named("gents/resolve_fixture_b@1.0.0", &options)
            .await
            .unwrap();
        assert_eq!(resolved.archive.digest(), v1.digest);
        let resolved = resolve_named("gents/resolve_fixture_b@2.0.0", &options)
            .await
            .unwrap();
        assert_eq!(resolved.archive.digest(), v2.digest);
    }

    #[tokio::test]
    async fn an_installed_record_is_preferred_over_a_different_stored_version() {
        let home = tempfile::tempdir().unwrap();
        let store = PackStore::new(home.path());
        let (older_bytes, older) = build_pack("resolve_fixture_c", "1.0.0");
        let (newer_bytes, _newer) = build_pack("resolve_fixture_c", "2.0.0");
        store.import(older_bytes.as_slice(), None).unwrap();
        store.import(newer_bytes.as_slice(), None).unwrap();

        let installed = [InstalledPack {
            coordinate: "gents/resolve_fixture_c".to_owned(),
            version: older.version.clone(),
            digest: older.digest.clone(),
        }];
        let options = ResolveOptions {
            home: Some(home.path()),
            registry_url: UNROUTABLE_REGISTRY.to_owned(),
            installed: &installed,
        };
        let resolved = resolve_named("gents/resolve_fixture_c", &options)
            .await
            .unwrap();
        assert!(matches!(resolved.from, ResolvedFrom::Installed));
        assert_eq!(
            resolved.archive.digest(),
            older.digest,
            "the installed version wins even though a newer one is stored"
        );
    }

    #[tokio::test]
    async fn a_stale_installed_record_falls_through_to_the_store() {
        let home = tempfile::tempdir().unwrap();
        let store = PackStore::new(home.path());
        let (bytes, header) = build_pack("resolve_fixture_d", "1.0.0");
        store.import(bytes.as_slice(), None).unwrap();

        let installed = [InstalledPack {
            coordinate: "gents/resolve_fixture_d".to_owned(),
            version: "0.9.0".to_owned(),
            digest: format!("sha256:{}", "0".repeat(64)),
        }];
        let options = ResolveOptions {
            home: Some(home.path()),
            registry_url: UNROUTABLE_REGISTRY.to_owned(),
            installed: &installed,
        };
        let resolved = resolve_named("gents/resolve_fixture_d", &options)
            .await
            .unwrap();
        assert!(matches!(resolved.from, ResolvedFrom::Store));
        assert_eq!(resolved.archive.digest(), header.digest);
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
        let message = format!("{}", ErrorChain(error.as_ref()));
        assert!(
            message.contains("is not in the pack store of")
                && message.contains("could not be reached"),
            "{message}"
        );
        assert!(
            message.contains("gents pack fetch gents/nowhere --store"),
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
        let message = format!("{}", ErrorChain(error.as_ref()));
        assert!(message.contains("gents/nowhere@9.9.9"), "{message}");
        assert!(
            message.contains("gents pack fetch gents/nowhere@9.9.9 --store"),
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
        let message = format!("{}", ErrorChain(error.as_ref()));
        assert!(
            message.contains("is not in the pack store of") && message.contains("has no such pack"),
            "{message}"
        );
    }
}
