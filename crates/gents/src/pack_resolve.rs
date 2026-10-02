//! Resolving a named pack: explicit local forms are the CLI's job
//! (`commands::pack::local::classify` in gents-cli), everything else comes
//! through here.
//!
//! [`parse_pack_spec`] is the one parser for `name`, `ns/name`, and either
//! with `@version` appended, so a bare name and a pinned coordinate mean the
//! same thing to every caller: the CLI, `self_config`, and dependency
//! resolution. [`resolve_named`] then tries, in order, an installed record
//! whose digest the store still holds, the store's name index (no network),
//! and finally the registry, which stores and indexes what it fetches so
//! the next resolution of that version is free of the network too.

use std::path::Path;

use anyhow::Result;

use crate::pack::{is_valid_pack_name, InstalledPack};
use crate::pack_archive::{PackArchive, DEFAULT_NAMESPACE};
use crate::pack_registry::{fetch_pack, RegistryClient, RegistryError};
use crate::pack_store::{is_valid_index_version, PackStore};

/// One parsed pack coordinate: `name` (this build's default namespace),
/// `ns/name`, either with `@version` appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackSpec<'a> {
    pub namespace: &'a str,
    pub name: &'a str,
    pub version: Option<&'a str>,
}

/// Parses `spec` into a [`PackSpec`]. Namespace and name must be
/// `is_valid_pack_name` (snake_case) and a pinned version must be
/// `is_valid_index_version`: they end up as path segments in the pack
/// store's name index, so a spec this parser refuses never has the chance to
/// become an unsafe one there, and the two checks can never disagree about
/// what a valid version looks like.
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

/// Where [`resolve_named`] found a pack.
#[derive(Debug, Clone)]
pub enum ResolvedFrom {
    /// The coordinate's installed record named this digest, and the store
    /// still holds it.
    Installed,
    /// The home's pack store held it; no network call was made.
    Store,
    /// Fetched from the registry, which stored and indexed it for next time.
    Registry {
        artifact_digest: String,
        version: String,
    },
}

/// A named pack, resolved and opened.
pub struct ResolvedNamedPack {
    pub archive: PackArchive,
    pub from: ResolvedFrom,
}

/// What [`resolve_named`] needs to resolve a spec.
pub struct ResolveOptions<'a> {
    /// The gents home whose store is consulted first. `None` skips straight
    /// to the registry and resolves entirely in memory, for a runtime
    /// caller (`self_config` without a home) that owns no store.
    pub home: Option<&'a Path>,
    /// From [`crate::pack_registry::resolve_registry_url`].
    pub registry_url: String,
    /// The coordinates already installed: file records always, node
    /// records too when the caller has node access. Order does not matter;
    /// every entry is checked.
    pub installed: &'a [InstalledPack],
}

/// Resolves a non-local pack spec: an installed record whose digest the
/// store still holds, then the store's name index, then the registry.
///
/// A caller with an explicit local form (`sha256:`, `./`, `../`, an
/// absolute path, `*.pack`) never reaches this: that classification is the
/// CLI's job, so this resolver only ever sees `name`, `ns/name`, or either
/// pinned with `@version`.
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

/// Turns a failed registry fetch into the one sentence an operator needs:
/// unreachable or not found, each naming the fix and keeping a pinned
/// `@version` in both the coordinate and the suggested command, so following
/// the advice resolves the same thing that failed. Classified by
/// [`RegistryError`], a typed marker from [`crate::pack_registry`], rather
/// than by matching on message text that module is free to reword. With no
/// home there is no local store to speak of, so the underlying error is
/// reported as is, with the coordinate for context.
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

    /// An unroutable loopback address: a connection to it fails immediately
    /// rather than timing out, so a test that must never reach the network
    /// still runs fast if it accidentally does.
    const UNROUTABLE_REGISTRY: &str = "http://127.0.0.1:1";

    /// `Result::unwrap_err` needs the `Ok` side to be `Debug`, which
    /// [`ResolvedNamedPack`] is not (it holds a [`PackArchive`], which
    /// carries no `Debug` impl either); this is the same check without that
    /// requirement.
    fn expect_err(result: Result<ResolvedNamedPack>) -> anyhow::Error {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => error,
        }
    }

    #[test]
    fn parse_pack_spec_handles_bare_namespaced_and_pinned_forms() {
        let spec = parse_pack_spec("demo").unwrap();
        assert_eq!(
            (spec.namespace, spec.name, spec.version),
            ("gents", "demo", None)
        );

        let spec = parse_pack_spec("acme/demo").unwrap();
        assert_eq!(
            (spec.namespace, spec.name, spec.version),
            ("acme", "demo", None)
        );

        let spec = parse_pack_spec("acme/demo@1.2.3").unwrap();
        assert_eq!(
            (spec.namespace, spec.name, spec.version),
            ("acme", "demo", Some("1.2.3"))
        );

        let spec = parse_pack_spec("demo@2").unwrap();
        assert_eq!(
            (spec.namespace, spec.name, spec.version),
            ("gents", "demo", Some("2"))
        );
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
            // A second `@` lands entirely inside what `split_once` reads as
            // the version, so only the version-shape check catches it.
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
        let message = format!("{error:#}");
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

    /// A pinned `@version` must survive into the offline sentence: dropping
    /// it would point the operator at `gents pack fetch gents/nowhere
    /// --store`, which fetches latest and does not fix the failed
    /// `@9.9.9` resolution.
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
            message.contains("gents pack fetch gents/nowhere@9.9.9 --store"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_registry_that_has_never_heard_of_the_pack_fails_with_the_404_sentence() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // No routes registered: axum's default fallback is a plain 404 for
        // any path, exactly what a registry that has never heard of this
        // pack would answer.
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
