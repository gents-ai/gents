//! The pack registry client: `gents pack search`, `gents pack publish`, and
//! the download-verify-cache path `gents pack install` falls back to when a
//! pack is not compiled into this binary.
//!
//! A pack fetched from the registry has to become the same pack a bundled
//! one is before it is trusted with anything: its raw bytes are checked
//! against the digest the registry advertised before they are ever parsed,
//! and only a match is cached and handed to [`gents::pack_archive::PackArchive`].

use anyhow::{Context, Result};
#[cfg(test)]
use gents::pack_archive::PackArchive;
pub(crate) use gents::pack_registry::{
    download_verified_pack, fetch_pack, resolve_pack_coordinate, resolve_registry_url,
    stage_and_persist, verify_digest, RegistryClient, RegistryPack,
};
#[cfg(test)]
use gents::pack_registry::{
    verify_pack_coordinate, RegistryKind, DEFAULT_REGISTRY_URL, REGISTRY_ENV_VAR,
};
#[cfg(test)]
use serde_json::Value;

use crate::cli::args::{PackFetchArgs, PackPublishArgs, PackSearchArgs};

const REGISTRY_TOKEN_ENV_VAR: &str = "GENTS_REGISTRY_TOKEN";

/// `--token` beats `GENTS_REGISTRY_TOKEN`; neither present is `None`.
pub(crate) fn resolve_registry_token(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var(REGISTRY_TOKEN_ENV_VAR)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

/// `gents pack fetch`: the artifact itself, verified, without installing
/// it.
///
/// Downloading a pack is the client's job, not something to hand off to
/// another tool. Publishing, installing and searching already go through
/// here; this closes the last case, so getting the bytes for an air-gapped
/// machine, a mirror, or a look inside never needs anything but `gents`.
///
/// The digest is checked against what the registry advertised before the
/// file is written, so what lands on disk is the pack that was asked for
/// or nothing at all.
pub(crate) async fn fetch(args: PackFetchArgs) -> Result<()> {
    let (namespace, name) = crate::commands::pack::split_namespace(&args.package);
    let base_url = resolve_registry_url(args.registry.as_deref());
    let client = RegistryClient::new(base_url.clone());

    let coordinate =
        resolve_pack_coordinate(&client, namespace, name, args.version.as_deref()).await?;
    let bytes = download_verified_pack(&client, &coordinate).await?;

    let out = args.out.unwrap_or_else(|| {
        std::path::PathBuf::from(format!("{name}-{}.tar.gz", coordinate.version))
    });
    std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    crate::print_json(&serde_json::json!({
        "pack": name,
        "namespace": namespace,
        "version": coordinate.version,
        "digest": coordinate.artifact_digest,
        "size_bytes": bytes.len(),
        "out": out.display().to_string(),
    }))
}

pub(crate) async fn search(args: PackSearchArgs) -> Result<()> {
    let client = RegistryClient::new(resolve_registry_url(args.registry.as_deref()));
    let results = client.search(args.query.as_deref().unwrap_or("")).await?;
    crate::print_json(&results)
}

pub(crate) async fn publish(args: PackPublishArgs) -> Result<()> {
    let token = resolve_registry_token(args.token.as_deref())
        .context("a registry token is required; pass --token or set GENTS_REGISTRY_TOKEN")?;
    let bytes =
        std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?;
    anyhow::ensure!(!bytes.is_empty(), "{} is empty", args.file.display());
    let client = RegistryClient::new(resolve_registry_url(args.registry.as_deref()));
    let result = client.publish(&token, bytes).await?;
    crate::print_json(&result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use axum::extract::{Path as AxumPath, State};
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::json;

    // --- env var precedence: guarded so parallel tests don't race the process env ---

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvVarGuard {
        /// Takes the lock once and clears the named variables, restoring
        /// them on drop.
        ///
        /// There is deliberately no second constructor that also takes the
        /// lock: a test that held one guard and then asked for another
        /// would wait forever on a lock it already holds. A test that
        /// wants to see a variable set calls [`Self::set`] on the guard it
        /// already has.
        fn clear(vars: &[&'static str]) -> Self {
            let lock = env_lock().lock().expect("env lock poisoned");
            let saved = vars
                .iter()
                .map(|name| (*name, std::env::var_os(name)))
                .collect();
            for name in vars {
                std::env::remove_var(name);
            }
            Self { saved, _lock: lock }
        }

        /// Sets a variable for as long as this guard lives. Its original
        /// value was saved when the guard cleared it, so the restore on
        /// drop still covers it.
        fn set(&self, name: &'static str, value: &str) {
            assert!(
                self.saved.iter().any(|(saved, _)| *saved == name),
                "{name} was not cleared by this guard, so its value would not be restored"
            );
            unsafe { std::env::set_var(name, value) };
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for (name, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn registry_url_precedence_is_flag_then_env_then_default() {
        let guard = EnvVarGuard::clear(&[REGISTRY_ENV_VAR]);
        assert_eq!(resolve_registry_url(None), DEFAULT_REGISTRY_URL);

        guard.set(REGISTRY_ENV_VAR, "https://env.example/");
        assert_eq!(resolve_registry_url(None), "https://env.example");
        assert_eq!(
            resolve_registry_url(Some("https://flag.example")),
            "https://flag.example"
        );
    }

    #[test]
    fn registry_token_precedence_is_flag_then_env() {
        let guard = EnvVarGuard::clear(&[REGISTRY_TOKEN_ENV_VAR]);
        assert_eq!(resolve_registry_token(None), None);

        guard.set(REGISTRY_TOKEN_ENV_VAR, "gcpat_env");
        assert_eq!(resolve_registry_token(None).as_deref(), Some("gcpat_env"));
        assert_eq!(
            resolve_registry_token(Some("gcpat_flag")).as_deref(),
            Some("gcpat_flag")
        );
    }

    // --- a tiny fake registry, just the routes `gents pack` needs ---

    struct FakeRegistryState {
        bytes: Vec<u8>,
        digest: String,
        downloads: Arc<AtomicUsize>,
    }

    /// Only "plain_pack" is known; anything else is a real 404, so the
    /// "unknown pack" test exercises a genuine not-found response instead
    /// of accidentally hitting the digest-mismatch path.
    async fn fake_package(AxumPath((_ns, name)): AxumPath<(String, String)>) -> Response {
        if name == "plain_pack" {
            Json(json!({ "latest": "1.0.0" })).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }

    async fn fake_version(State(state): State<Arc<FakeRegistryState>>) -> Json<Value> {
        Json(json!({ "digest": state.digest }))
    }

    async fn fake_download(
        State(state): State<Arc<FakeRegistryState>>,
        AxumPath(_): AxumPath<(String, String, String)>,
    ) -> impl IntoResponse {
        state.downloads.fetch_add(1, Ordering::SeqCst);
        state.bytes.clone()
    }

    async fn fake_oversized_download() -> Response {
        let body = vec![0_u8; gents::pack_archive::MAX_PACK_BYTES + 1];
        Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_LENGTH, body.len().to_string())
            .body(axum::body::Body::from(body))
            .unwrap()
    }

    /// The two surfaces the registry actually serves, asserted by name.
    ///
    /// A client that asks the wrong one gets a correct 404 and an error
    /// that says the registry has nothing there, which reads like a
    /// missing package rather than a wrong route. That is what happened:
    /// `gents pack install` asked the plugin surface for a pack. The route
    /// segments are stated here, next to the fakes that must match them,
    /// so swapping them fails a test instead of a customer's install.
    #[test]
    fn each_artifact_kind_addresses_its_own_registry_surface() {
        assert_eq!(RegistryKind::Pack.path(), "packs");
        assert_eq!(RegistryKind::Plugin.path(), "packages");
    }

    /// Starts a fake registry serving one version of one pack, and returns
    /// its base URL plus the download-hit counter.
    async fn start_fake_registry(
        bytes: Vec<u8>,
        advertised_digest: String,
    ) -> (String, Arc<AtomicUsize>) {
        let downloads = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(FakeRegistryState {
            bytes,
            digest: advertised_digest,
            downloads: downloads.clone(),
        });
        // `/packs/...`, the route the real registry serves a pack on.
        // These routes had been the plugin ones (`/packages/...`), which is
        // how a client that asked the wrong surface for a pack passed every
        // test in this file while failing against a real server.
        let app = Router::new()
            .route("/api/v1/packs/{ns}/{name}", get(fake_package))
            .route("/api/v1/packs/{ns}/{name}/{version}", get(fake_version))
            .route(
                "/api/v1/packs/{ns}/{name}/{version}/download",
                get(fake_download),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fake registry");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), downloads)
    }

    /// A tiny real `.tar.gz`, built the same way `gents pack build` does,
    /// so these tests exercise the real container rather than a stand-in.
    fn sample_pack() -> (Vec<u8>, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("plain_pack");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plain pack").unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": "plain_pack",
            "version": "1.0.0",
            "description": "A plain pack with no plugins",
            "authors": ["gents-ai contributors"],
            "tags": ["example"],
            "kind": "assets",
            "assets": ["README.md"],
            "dependencies": [],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        gents::pack_archive::pack_dir(&root).unwrap()
    }

    #[test]
    fn a_pack_manifest_must_match_the_requested_coordinate() {
        let (bytes, _) = sample_pack();
        let archive = PackArchive::from_bytes(&bytes).unwrap();
        let manifest = archive.manifest();

        verify_pack_coordinate(manifest, "gents", "plain_pack", "1.0.0").unwrap();
        for (namespace, name, version) in [
            ("someone_else", "plain_pack", "1.0.0"),
            ("gents", "another_pack", "1.0.0"),
            ("gents", "plain_pack", "2.0.0"),
        ] {
            let error = verify_pack_coordinate(manifest, namespace, name, version)
                .expect_err("every coordinate component is identity-bearing");
            let message = format!("{error:#}");
            assert!(message.contains("different identity"), "{message}");
            assert!(
                message.contains(&format!("{namespace}/{name}@{version}")),
                "{message}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_pack_downloads_verifies_and_caches() {
        let (bytes, digest) = sample_pack();
        let (base_url, downloads) = start_fake_registry(bytes.clone(), digest.clone()).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let first = fetch_pack(&client, Some(home.path()), "gents", "plain_pack")
            .await
            .expect("first fetch");
        assert_eq!(first.namespace, "gents");
        assert_eq!(first.name, "plain_pack");
        assert_eq!(first.version, "1.0.0");
        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        assert!(home
            .path()
            .join("packs")
            .join("registry-cache")
            .join(format!("{digest}.tar.gz"))
            .is_file());

        // A second fetch of the same pack reads the cache: no second download.
        let second = fetch_pack(&client, Some(home.path()), "gents", "plain_pack")
            .await
            .expect("second fetch (cached)");
        assert_eq!(second.digest, first.digest);
        assert_eq!(
            downloads.load(Ordering::SeqCst),
            1,
            "second install must not refetch"
        );
    }

    #[tokio::test]
    async fn a_digest_mismatch_is_refused_naming_both_digests() {
        let (bytes, _real_digest) = sample_pack();
        let wrong_digest = "0".repeat(64);
        let (base_url, _downloads) = start_fake_registry(bytes, wrong_digest.clone()).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let error = fetch_pack(&client, Some(home.path()), "gents", "plain_pack")
            .await
            .expect_err("mismatched digest must be refused");
        let message = format!("{error:#}");
        assert!(message.contains(&wrong_digest), "{message}");
        assert!(message.contains("refusing to install"), "{message}");
        // Nothing was cached under the wrong name: a refusal is not a warning.
        assert!(!home
            .path()
            .join("packs")
            .join("registry-cache")
            .join(format!("{wrong_digest}.tar.gz"))
            .exists());
    }

    #[tokio::test]
    async fn a_coordinate_mismatch_is_refused_before_the_pack_is_cached() {
        let (bytes, digest) = sample_pack();
        let (base_url, _downloads) = start_fake_registry(bytes, digest.clone()).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let error = fetch_pack(&client, Some(home.path()), "someone_else", "plain_pack")
            .await
            .expect_err("a manifest from another namespace must be refused");
        assert!(
            format!("{error:#}").contains("different identity"),
            "{error:#}"
        );
        assert!(!home
            .path()
            .join("packs")
            .join("registry-cache")
            .join(format!("{digest}.tar.gz"))
            .exists());
    }

    #[tokio::test]
    async fn an_unknown_pack_is_a_clean_not_found_not_a_silent_success() {
        let (bytes, digest) = sample_pack();
        let (base_url, _downloads) = start_fake_registry(bytes, digest).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let error = fetch_pack(&client, Some(home.path()), "gents", "does_not_exist")
            .await
            .expect_err("unknown pack must be refused, not silently substituted");
        assert!(format!("{error:#}").contains("nothing at"), "{error:#}");
    }

    #[tokio::test]
    async fn download_rejects_an_oversized_body_before_buffering_it() {
        let app = Router::new().route(
            "/api/v1/packs/{ns}/{name}/{version}/download",
            get(fake_oversized_download),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = RegistryClient::new(format!("http://{address}"));
        let error = client
            .download("gents", "oversized", "1.0.0")
            .await
            .expect_err("advertised bodies over the archive limit must not be buffered");
        assert!(
            format!("{error:#}").contains("compressed bound"),
            "{error:#}"
        );
    }
}
