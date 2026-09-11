//! The pack registry client: `gents pack search`, `gents pack publish`, and
//! the download-verify-cache path `gents pack install` falls back to when a
//! pack is not compiled into this binary.
//!
//! A pack fetched from the registry has to become the same pack a bundled
//! one is before it is trusted with anything: its raw bytes are checked
//! against the digest the registry advertised before they are ever parsed,
//! and only a match is cached and handed to [`gents::pack_archive::PackArchive`].

use std::path::Path;

use anyhow::{Context, Result};
use gents::pack_archive::PackArchive;
use serde_json::Value;

use crate::cli::args::{PackFetchArgs, PackPublishArgs, PackSearchArgs};

/// The registry every pack this project publishes lives on, and the
/// fallback used when neither `--registry` nor `GENTS_REGISTRY` names one.
pub(crate) const DEFAULT_REGISTRY_URL: &str = "https://packs.gents.xyz";
const REGISTRY_ENV_VAR: &str = "GENTS_REGISTRY";
const REGISTRY_TOKEN_ENV_VAR: &str = "GENTS_REGISTRY_TOKEN";

/// `--registry` beats `GENTS_REGISTRY` beats the default, in that order.
pub(crate) fn resolve_registry_url(explicit: Option<&str>) -> String {
    for candidate in [
        explicit.map(str::to_owned),
        std::env::var(REGISTRY_ENV_VAR).ok(),
    ] {
        if let Some(url) = candidate.filter(|value| !value.trim().is_empty()) {
            return url.trim_end_matches('/').to_owned();
        }
    }
    DEFAULT_REGISTRY_URL.to_owned()
}

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

/// A thin HTTP/JSON client for the routes `gents pack` needs, matching the
/// registry's `/api/v1` surface: package metadata, version metadata,
/// search, download, and publish.
pub(crate) struct RegistryClient {
    base_url: String,
    /// Which of the registry's two surfaces this client addresses.
    kind: RegistryKind,
    http: reqwest::Client,
}

/// Which of the registry's two artifact surfaces a client talks to.
///
/// The registry serves the same shape twice, once per kind: a plugin lives
/// under `/packages/...` and a pack under `/packs/...`. A client that
/// guessed one for both would ask for a pack on the plugin route and be
/// told, correctly, that there is nothing there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistryKind {
    Pack,
    Plugin,
}

impl RegistryKind {
    fn path(self) -> &'static str {
        match self {
            Self::Pack => "packs",
            Self::Plugin => "packages",
        }
    }
}

impl RegistryClient {
    /// A client for the pack surface.
    pub(crate) fn new(base_url: String) -> Self {
        Self::for_kind(base_url, RegistryKind::Pack)
    }

    /// A client for the plugin surface.
    pub(crate) fn for_plugins(base_url: String) -> Self {
        Self::for_kind(base_url, RegistryKind::Plugin)
    }

    fn for_kind(base_url: String, kind: RegistryKind) -> Self {
        Self {
            base_url,
            kind,
            http: reqwest::Client::new(),
        }
    }

    /// The command to run by hand when this client could not reach the
    /// registry at all.
    ///
    /// A request can fail for reasons that have nothing to do with the
    /// registry or the pack: a proxy, a certificate store, an air gap, a
    /// machine that is simply offline. Naming the exact equivalent turns
    /// "it did not work" into something the person reading it can run,
    /// and lets them hand the bytes back to `gents pack install <file>`.
    fn curl_equivalent(url: &str, authenticated: bool) -> String {
        let auth = if authenticated {
            " -H \"Authorization: Bearer $GENTS_REGISTRY_TOKEN\""
        } else {
            ""
        };
        format!("curl -fsSL{auth} {url}")
    }

    fn api(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base_url)
    }

    async fn json_or_error(response: reqwest::Response, url: &str) -> Result<Value> {
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("the registry has nothing at {url}");
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("the registry rejected the request to {url} ({status}): {body}");
        }
        response
            .json::<Value>()
            .await
            .with_context(|| format!("reading the registry's response from {url}"))
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let url = self.api(path);
        let response = self.http.get(&url).send().await.with_context(|| {
            format!(
                "requesting {url}; if this machine cannot reach the registry, the same \
                     request by hand is: {}",
                Self::curl_equivalent(&url, false)
            )
        })?;
        Self::json_or_error(response, &url).await
    }

    pub(crate) async fn package(&self, namespace: &str, name: &str) -> Result<Value> {
        let kind = self.kind.path();
        self.get_json(&format!("/{kind}/{namespace}/{name}")).await
    }

    pub(crate) async fn version(
        &self,
        namespace: &str,
        name: &str,
        version: &str,
    ) -> Result<Value> {
        let kind = self.kind.path();
        self.get_json(&format!("/{kind}/{namespace}/{name}/{version}"))
            .await
    }

    pub(crate) async fn search(&self, query: &str) -> Result<Value> {
        let url = self.api(&format!("/{}", self.kind.path()));
        let response = self
            .http
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .with_context(|| {
                format!(
                    "requesting {url}; if this machine cannot reach the registry, the same \
                     request by hand is: {}",
                    Self::curl_equivalent(&url, false)
                )
            })?;
        Self::json_or_error(response, &url).await
    }

    pub(crate) async fn download(
        &self,
        namespace: &str,
        name: &str,
        version: &str,
    ) -> Result<Vec<u8>> {
        let url = self.api(&format!(
            "/{}/{namespace}/{name}/{version}/download",
            self.kind.path()
        ));
        let response = self.http.get(&url).send().await.with_context(|| {
            format!(
                "downloading {url}; if this machine cannot reach the registry, fetch it by \
                     hand with `{} -o {name}-{version}.tar.gz` and install that file",
                Self::curl_equivalent(&url, false)
            )
        })?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "downloading {namespace}/{name}@{version} from the registry failed ({status})"
        );
        Ok(response
            .bytes()
            .await
            .with_context(|| format!("reading the download body from {url}"))?
            .to_vec())
    }

    pub(crate) async fn publish(&self, token: &str, bytes: Vec<u8>) -> Result<Value> {
        let url = self.api("/publish");
        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("publishing to {url}"))?;
        Self::json_or_error(response, &url).await
    }
}

/// A pack downloaded from the registry, already verified and parsed: the
/// same pack an install from this binary would use, wherever it came from.
#[derive(Debug)]
pub(crate) struct RegistryPack {
    pub(crate) archive: PackArchive,
    pub(crate) digest: String,
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) version: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// The one check every route to `bytes` has to pass, before the bytes are
/// trusted with anything else: their own hash names both digests on a
/// mismatch, and it is a refusal, never a warning.
pub(crate) fn verify_digest(bytes: &[u8], advertised: &str, coordinate: &str) -> Result<()> {
    let computed = sha256_hex(bytes);
    anyhow::ensure!(
        computed == advertised,
        "the registry advertised digest {advertised} for {coordinate} but the bytes hash to \
         {computed}; refusing to install a pack that does not match what the registry described"
    );
    Ok(())
}

/// Downloads `namespace/name`'s latest version, checks it against the
/// digest the registry advertised before opening it, caches the verified
/// bytes content-addressed under `home`, and parses the result.
///
/// A second call for the same pack reads the cache instead of refetching;
/// the digest is still checked either way, so a tampered cache file is
/// caught rather than trusted.
pub(crate) async fn fetch_pack(
    client: &RegistryClient,
    home: &Path,
    namespace: &str,
    name: &str,
) -> Result<RegistryPack> {
    let package = client
        .package(namespace, name)
        .await
        .with_context(|| format!("looking up {namespace}/{name} on the registry"))?;
    let version = package
        .get("latest")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("{namespace}/{name} has no published version yet"))?
        .to_owned();
    let version_info = client.version(namespace, name, &version).await?;
    let advertised = version_info
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the registry did not advertise a digest for {namespace}/{name}@{version}"
            )
        })?
        .to_owned();

    let cache_dir = home.join("packs").join("registry-cache");
    std::fs::create_dir_all(&cache_dir).with_context(|| {
        format!(
            "creating the pack download cache under {}",
            cache_dir.display()
        )
    })?;
    let cache_path = cache_dir.join(format!("{advertised}.tar.gz"));
    let coordinate = format!("{namespace}/{name}@{version}");

    let bytes = if cache_path.is_file() {
        let cached = std::fs::read(&cache_path)
            .with_context(|| format!("reading the cached pack {}", cache_path.display()))?;
        verify_digest(&cached, &advertised, &coordinate)?;
        cached
    } else {
        // Verify before the bytes ever touch the cache: a pack that fails
        // the check is refused outright, never staged under the digest it
        // did not earn.
        let downloaded = client.download(namespace, name, &version).await?;
        verify_digest(&downloaded, &advertised, &coordinate)?;
        stage_and_persist(&cache_dir, &cache_path, &downloaded)?;
        downloaded
    };

    let archive = PackArchive::from_bytes(&bytes).with_context(|| {
        format!("{namespace}/{name}@{version} from the registry is not a readable pack")
    })?;
    let digest = archive.digest().with_context(|| {
        format!("{namespace}/{name}@{version} from the registry failed its own content check")
    })?;
    Ok(RegistryPack {
        archive,
        digest,
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        version,
    })
}

/// Stages the download beside its content-addressed destination and
/// publishes it without ever exposing a partial file. The destination name
/// is the content's own digest, so an existing file there is already the
/// right bytes; a fresh write races safely against a concurrent install of
/// the same pack.
pub(crate) fn stage_and_persist(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut staged = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("staging the download in {}", dir.display()))?;
    staged
        .write_all(bytes)
        .context("writing the staged download")?;
    staged.as_file().sync_all().ok();
    match staged.persist_noclobber(dest) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.error)
            .with_context(|| format!("saving the downloaded pack to {}", dest.display())),
    }
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

    let version = match args.version {
        Some(version) => version,
        None => {
            let package = client
                .package(namespace, name)
                .await
                .with_context(|| format!("looking up {namespace}/{name} on {base_url}"))?;
            package
                .get("latest")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("{namespace}/{name} has no published version yet"))?
                .to_owned()
        }
    };

    let version_info = client.version(namespace, name, &version).await?;
    let advertised = version_info
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the registry did not advertise a digest for {namespace}/{name}@{version}"
            )
        })?
        .to_owned();

    let coordinate = format!("{namespace}/{name}@{version}");
    let bytes = client.download(namespace, name, &version).await?;
    verify_digest(&bytes, &advertised, &coordinate)?;

    let out = args
        .out
        .unwrap_or_else(|| std::path::PathBuf::from(format!("{name}-{version}.tar.gz")));
    std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    crate::print_json(&serde_json::json!({
        "pack": name,
        "namespace": namespace,
        "version": version,
        "digest": advertised,
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

    #[tokio::test]
    async fn fetch_pack_downloads_verifies_and_caches() {
        let (bytes, digest) = sample_pack();
        let (base_url, downloads) = start_fake_registry(bytes.clone(), digest.clone()).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let first = fetch_pack(&client, home.path(), "gents", "plain_pack")
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
        let second = fetch_pack(&client, home.path(), "gents", "plain_pack")
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

        let error = fetch_pack(&client, home.path(), "gents", "plain_pack")
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
    async fn an_unknown_pack_is_a_clean_not_found_not_a_silent_success() {
        let (bytes, digest) = sample_pack();
        let (base_url, _downloads) = start_fake_registry(bytes, digest).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let error = fetch_pack(&client, home.path(), "gents", "does_not_exist")
            .await
            .expect_err("unknown pack must be refused, not silently substituted");
        assert!(format!("{error:#}").contains("nothing at"), "{error:#}");
    }
}
