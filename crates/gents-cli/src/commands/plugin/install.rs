//! `gents plugin install`: download a plugin from the registry, verify it
//! against the digest the registry advertised before it is opened, cache
//! it content-addressed under the home, and record it as installed so
//! `gents plugin run` and `gents plugin list` can find it by name.

use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::args::PluginInstallArgs;
use crate::commands::pack::registry::{resolve_registry_url, verify_digest, RegistryClient};

use super::store::{self, InstalledPlugin};

pub(crate) async fn install(args: PluginInstallArgs) -> Result<()> {
    let (namespace, name) = crate::commands::pack::split_namespace(&args.name);
    let base_url = resolve_registry_url(args.registry.as_deref());
    // The plugin surface, not the pack one: same shapes, different route.
    let client = RegistryClient::for_plugins(base_url.clone());

    let version = match args.version {
        Some(version) => version,
        None => {
            let package = client
                .package(namespace, name)
                .await
                .with_context(|| format!("looking up {namespace}/{name} on {base_url}"))?;
            package
                .get("latest")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("{namespace}/{name} has no published version yet"))?
                .to_owned()
        }
    };

    let version_info = client.version(namespace, name, &version).await?;
    let advertised = version_info
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the registry did not advertise a digest for {namespace}/{name}@{version}"
            )
        })?
        .to_owned();

    // Verify before the bytes are ever opened: a mismatch is refused
    // outright, never parsed on the assumption it is close enough.
    let bytes = client.download(namespace, name, &version).await?;
    verify_digest(
        &bytes,
        &advertised,
        &format!("{namespace}/{name}@{version}"),
    )?;

    let afb = afterburner_cloud::Afb::from_bytes(&bytes).with_context(|| {
        format!("{namespace}/{name}@{version} from the registry is not a readable plugin")
    })?;

    // The registry advertises (and `verify_digest` checks) the bare
    // artifact digest, matching `Afb::artifact_digest`'s own bare-hex
    // format - never "sha256:"-prefixed. This module's own store records
    // the prefixed form for a human reading `gents plugin list`, so the
    // prefix is added here rather than assumed already present.
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    store::store_bytes(&home, &advertised, &bytes)?;

    let record = InstalledPlugin {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        version,
        digest: format!("sha256:{advertised}"),
        language: afb.manifest.package.language.clone(),
    };
    store::write_record(&home, &record)?;

    crate::print_json(&json!({
        "namespace": record.namespace,
        "name": record.name,
        "version": record.version,
        "digest": record.digest,
        "language": record.language,
        "home": home,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path as AxumPath, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// A real compiled plugin and the bare-hex digest a registry would
    /// advertise for it (`verify_digest` compares against `sha256_hex`'s
    /// own bare output, never the `sha256:`-prefixed form this module's
    /// store records). The shared fixture compiles it exactly as `gents
    /// plugin build` does, so these tests exercise the real container
    /// rather than a stand-in.
    fn sample_plugin_afb() -> (Vec<u8>, String) {
        let bytes = crate::commands::plugin::testing::build_plugin_afb(
            "echo",
            b"fn main() { println!(\"ok\"); }",
        );
        let digest = crate::commands::plugin::testing::digest_of(&bytes);
        (bytes, digest)
    }

    struct FakeRegistryState {
        bytes: Vec<u8>,
        digest: String,
        downloads: Arc<AtomicUsize>,
    }

    async fn fake_package(AxumPath((_ns, name)): AxumPath<(String, String)>) -> Response {
        if name == "echo" {
            Json(serde_json::json!({ "latest": "0.1.0" })).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }

    async fn fake_version(State(state): State<Arc<FakeRegistryState>>) -> Json<Value> {
        Json(serde_json::json!({ "digest": state.digest }))
    }

    async fn fake_download(
        State(state): State<Arc<FakeRegistryState>>,
        AxumPath(_): AxumPath<(String, String, String)>,
    ) -> impl IntoResponse {
        state.downloads.fetch_add(1, Ordering::SeqCst);
        state.bytes.clone()
    }

    async fn start_fake_registry(bytes: Vec<u8>, digest: String) -> (String, Arc<AtomicUsize>) {
        let downloads = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(FakeRegistryState {
            bytes,
            digest,
            downloads: downloads.clone(),
        });
        let app = Router::new()
            .route("/api/v1/packages/{ns}/{name}", get(fake_package))
            .route("/api/v1/packages/{ns}/{name}/{version}", get(fake_version))
            .route(
                "/api/v1/packages/{ns}/{name}/{version}/download",
                get(fake_download),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), downloads)
    }

    #[tokio::test]
    async fn install_downloads_verifies_caches_and_records() {
        let (bytes, digest) = sample_plugin_afb();
        let (base_url, downloads) = start_fake_registry(bytes.clone(), digest.clone()).await;
        let home = tempfile::tempdir().unwrap();

        install(PluginInstallArgs {
            name: "gents/echo".to_owned(),
            version: None,
            registry: Some(base_url),
            home: Some(home.path().to_owned()),
        })
        .await
        .expect("install must succeed");

        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        let record = store::read_record(home.path(), "gents", "echo").expect("record written");
        assert_eq!(record.version, "0.1.0");
        assert_eq!(record.digest, format!("sha256:{digest}"));
        assert_eq!(record.language, "rust");
        assert_eq!(store::read_bytes(home.path(), &digest).unwrap(), bytes);
    }

    #[tokio::test]
    async fn a_digest_mismatch_is_refused_naming_both_digests() {
        let (bytes, _real_digest) = sample_plugin_afb();
        let wrong_digest = "0".repeat(64);
        let (base_url, _downloads) = start_fake_registry(bytes, wrong_digest.clone()).await;
        let home = tempfile::tempdir().unwrap();

        let error = install(PluginInstallArgs {
            name: "gents/echo".to_owned(),
            version: None,
            registry: Some(base_url),
            home: Some(home.path().to_owned()),
        })
        .await
        .expect_err("a digest mismatch must be refused");
        let message = format!("{error:#}");
        assert!(message.contains(&wrong_digest), "{message}");
        assert!(message.contains("refusing to install"), "{message}");
        assert!(
            !store::read_record(home.path(), "gents", "echo").is_ok(),
            "nothing must be recorded on a refused install"
        );
    }

    #[tokio::test]
    async fn install_then_remove_round_trips() {
        let (bytes, digest) = sample_plugin_afb();
        let (base_url, _downloads) = start_fake_registry(bytes, digest.clone()).await;
        let home = tempfile::tempdir().unwrap();

        install(PluginInstallArgs {
            name: "gents/echo".to_owned(),
            version: None,
            registry: Some(base_url),
            home: Some(home.path().to_owned()),
        })
        .await
        .unwrap();
        assert!(store::read_record(home.path(), "gents", "echo").is_ok());

        let removed = store::remove_record(home.path(), "gents", "echo").expect("remove");
        assert_eq!(removed.digest, format!("sha256:{digest}"));
        assert!(
            store::read_record(home.path(), "gents", "echo").is_err(),
            "the record must be gone after remove"
        );
        // The content store is untouched: removing a name never deletes
        // bytes another install might still name (this module's own doc).
        assert!(store::read_bytes(home.path(), &digest).is_ok());
    }

    #[tokio::test]
    async fn list_reflects_what_was_installed() {
        let (bytes, digest) = sample_plugin_afb();
        let (base_url, _downloads) = start_fake_registry(bytes, digest).await;
        let home = tempfile::tempdir().unwrap();

        assert!(store::list_records(home.path()).unwrap().is_empty());

        install(PluginInstallArgs {
            name: "gents/echo".to_owned(),
            version: None,
            registry: Some(base_url),
            home: Some(home.path().to_owned()),
        })
        .await
        .unwrap();

        let records = store::list_records(home.path()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].namespace, "gents");
        assert_eq!(records[0].name, "echo");
        assert_eq!(records[0].language, "rust");
    }
}
