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
    fetch_pack, resolve_pack_coordinate, resolve_registry_url, RegistryClient, RegistryPack,
};
#[cfg(test)]
use gents::pack_registry::{verify_pack_coordinate, DEFAULT_REGISTRY_URL, REGISTRY_ENV_VAR};
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

/// Reads one line from standard input, trimming the trailing newline. Shared
/// by `--token-stdin` and `--password-stdin`, so a token or password never
/// shows up in `ps` or shell history.
pub(crate) fn read_stdin_line(what: &str) -> Result<String> {
    use std::io::BufRead;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .with_context(|| format!("reading the {what} from standard input"))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// The `--token` value, read from standard input instead when `--token-stdin`
/// was passed. One function so every command that accepts both flags agrees
/// on how the token is obtained.
pub(crate) fn resolve_token_flag(
    token: Option<String>,
    token_stdin: bool,
) -> Result<Option<String>> {
    if token_stdin {
        Ok(Some(read_stdin_line("registry token")?))
    } else {
        Ok(token)
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

    let coordinate =
        resolve_pack_coordinate(&client, namespace, name, args.version.as_deref()).await?;
    // Streamed to a staging file beside the output, verified, then renamed:
    // a pack of any size is never held in memory.
    let out_dir = args
        .out
        .as_deref()
        .and_then(std::path::Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(
            || std::path::PathBuf::from("."),
            std::path::Path::to_path_buf,
        );
    let mut staged = tempfile::NamedTempFile::new_in(&out_dir)
        .with_context(|| format!("staging the download in {}", out_dir.display()))?;
    let computed = {
        let mut writer = std::io::BufWriter::new(staged.as_file_mut());
        let computed = client
            .download_into(namespace, name, &coordinate.version, &mut writer)
            .await?;
        std::io::Write::flush(&mut writer).context("saving the download")?;
        computed
    };
    verify_digest_hex(&computed, &coordinate.artifact_digest, &coordinate.version)?;
    let header = gents::pack_archive::read_pack(
        std::io::BufReader::new(std::fs::File::open(staged.path())?),
        gents::pack_archive::Bounds::default(),
        |_, _| Ok(()),
    )
    .with_context(|| format!("{namespace}/{name} from the registry is not a readable pack"))?
    .header;
    let size_bytes = staged.as_file().metadata()?.len();
    let out = args
        .out
        .unwrap_or_else(|| std::path::PathBuf::from(header.file_name()));
    staged
        .persist(&out)
        .map_err(|error| error.error)
        .with_context(|| format!("writing {}", out.display()))?;

    crate::print_json(&serde_json::json!({
        "pack": name,
        "namespace": namespace,
        "version": coordinate.version,
        "digest": header.digest,
        "size_bytes": size_bytes,
        "out": out.display().to_string(),
    }))
}

/// The registry's advertised digest must be what the bytes hash to.
fn verify_digest_hex(computed: &str, advertised: &str, version: &str) -> Result<()> {
    anyhow::ensure!(
        computed == advertised,
        "the registry advertised digest {advertised} for version {version} but the bytes hash to {computed}; nothing was saved"
    );
    Ok(())
}

pub(crate) async fn search(args: PackSearchArgs) -> Result<()> {
    let client = RegistryClient::new(resolve_registry_url(args.registry.as_deref()));
    let results = client
        .search(args.query.as_deref().unwrap_or(""), args.page.max(1))
        .await?;
    crate::print_json(&results)
}

pub(crate) async fn publish(args: PackPublishArgs) -> Result<()> {
    let registry = resolve_registry_url(args.registry.as_deref());
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let token_flag = resolve_token_flag(args.token, args.token_stdin)?;
    let token = super::account::resolve_publish_token(token_flag.as_deref(), &registry, &home)?;
    let bytes =
        std::fs::read(&args.file).with_context(|| format!("reading {}", args.file.display()))?;
    anyhow::ensure!(!bytes.is_empty(), "{} is empty", args.file.display());
    let client = RegistryClient::new(registry);
    let result = client.publish(&token, bytes).await?;
    crate::print_json(&result)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

    use axum::extract::{Path as AxumPath, Query, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::json;

    /// The account and token every fake registry in this module accepts.
    pub(crate) const FAKE_USERNAME: &str = "demo";
    pub(crate) const FAKE_PASSWORD: &str = "demo-pass";
    pub(crate) const FAKE_TOKEN: &str = "demo-token";

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

    /// A write resolves its token from the flag, then the environment, then
    /// the login `gents pack login` saved for that registry.
    #[test]
    fn a_saved_login_is_the_last_token_source() {
        let guard = EnvVarGuard::clear(&[REGISTRY_TOKEN_ENV_VAR]);
        let home = tempfile::tempdir().unwrap();
        let registry = "https://registry.example";
        let resolve = |explicit| {
            super::super::account::resolve_publish_token(explicit, registry, home.path())
        };
        let error = resolve(None).unwrap_err();
        assert!(
            format!("{error:#}").contains("gents pack login"),
            "{error:#}"
        );
        gents::pack_registry::credentials::set(home.path(), registry, "gcpat_saved").unwrap();
        assert_eq!(resolve(None).unwrap(), "gcpat_saved");
        guard.set(REGISTRY_TOKEN_ENV_VAR, "gcpat_env");
        assert_eq!(resolve(None).unwrap(), "gcpat_env");
        assert_eq!(resolve(Some("gcpat_flag")).unwrap(), "gcpat_flag");
    }

    // --- a tiny fake registry, just the routes `gents pack` needs ---

    /// `(namespace, name, version, undo, bearer_token)` of a yank request.
    pub(crate) type YankRequest = (String, String, String, bool, String);
    /// `(namespace, name, new_owner, bearer_token)` of an owner transfer.
    pub(crate) type OwnerTransferRequest = (String, String, String, String);

    pub(crate) struct FakeRegistryState {
        name: String,
        latest: String,
        bytes: Vec<u8>,
        digest: String,
        pub(crate) downloads: AtomicUsize,
        /// `(namespace, name, version, undo, bearer_token)` from the last
        /// yank request.
        pub(crate) last_yank: Mutex<Option<YankRequest>>,
        /// `(namespace, name, new_owner, bearer_token)` from the last owner
        /// transfer.
        pub(crate) last_owner_transfer: Mutex<Option<OwnerTransferRequest>>,
    }

    /// Only the one pack is known; anything else is a real 404, so an
    /// "unknown pack" test exercises a genuine not-found response instead
    /// of accidentally hitting the digest-mismatch path.
    async fn fake_package(
        State(state): State<Arc<FakeRegistryState>>,
        AxumPath((_ns, name)): AxumPath<(String, String)>,
    ) -> Response {
        if name == state.name {
            Json(json!({ "latest": state.latest })).into_response()
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

    fn bearer_token(headers: &HeaderMap) -> Option<String> {
        headers
            .get(axum::http::header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")
            .map(str::to_owned)
    }

    async fn fake_login(Json(body): Json<Value>) -> Response {
        if body["username"].as_str() == Some(FAKE_USERNAME)
            && body["password"].as_str() == Some(FAKE_PASSWORD)
        {
            Json(json!({ "token": FAKE_TOKEN })).into_response()
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        }
    }

    async fn fake_me(headers: HeaderMap) -> Response {
        if bearer_token(&headers).as_deref() == Some(FAKE_TOKEN) {
            Json(json!({ "username": FAKE_USERNAME })).into_response()
        } else {
            StatusCode::UNAUTHORIZED.into_response()
        }
    }

    async fn fake_yank(
        State(state): State<Arc<FakeRegistryState>>,
        AxumPath((ns, name, version)): AxumPath<(String, String, String)>,
        Query(params): Query<BTreeMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        let Some(token) = bearer_token(&headers) else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        let undo = params.get("undo").map(String::as_str) == Some("true");
        *state.last_yank.lock().unwrap() = Some((ns, name, version.clone(), undo, token));
        Json(json!({ "version": version, "yanked": !undo })).into_response()
    }

    async fn fake_owner(
        State(state): State<Arc<FakeRegistryState>>,
        AxumPath((ns, name)): AxumPath<(String, String)>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let Some(token) = bearer_token(&headers) else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        let new_owner = body["username"].as_str().unwrap_or_default().to_owned();
        *state.last_owner_transfer.lock().unwrap() = Some((ns, name, new_owner.clone(), token));
        Json(json!({ "owner": new_owner })).into_response()
    }

    /// Starts a fake registry serving `plain_pack@1.0.0`.
    async fn start_fake_registry(
        bytes: Vec<u8>,
        advertised_digest: String,
    ) -> (String, Arc<FakeRegistryState>) {
        serve_fake_pack("plain_pack", "1.0.0", bytes, advertised_digest).await
    }

    /// Starts a fake registry serving one version of one pack, plus the
    /// account routes (`login`, `me`, `yank`, `owner`) every registry token
    /// command needs; all accept [`FAKE_TOKEN`].
    pub(crate) async fn serve_fake_pack(
        name: &str,
        latest: &str,
        bytes: Vec<u8>,
        advertised_digest: String,
    ) -> (String, Arc<FakeRegistryState>) {
        let state = Arc::new(FakeRegistryState {
            name: name.to_owned(),
            latest: latest.to_owned(),
            bytes,
            digest: advertised_digest,
            downloads: AtomicUsize::new(0),
            last_yank: Mutex::new(None),
            last_owner_transfer: Mutex::new(None),
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
            .route("/api/v1/packs/{ns}/{name}/{version}/yank", post(fake_yank))
            .route("/api/v1/packages/{ns}/{name}/owner", post(fake_owner))
            .route("/api/v1/login", post(fake_login))
            .route("/api/v1/me", get(fake_me))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind fake registry");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), state)
    }

    /// A tiny real `.pack`, built the same way `gents pack build` does, with
    /// the digest of its bytes the registry advertises.
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
        let (bytes, _) = gents::pack_archive::pack_dir(&root).unwrap();
        let digest = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(&bytes))
        };
        (bytes, digest)
    }

    /// How many packs the home's store holds.
    fn stored_packs(home: &std::path::Path) -> usize {
        std::fs::read_dir(home.join("packs/store/sha256"))
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
                    .count()
            })
            .unwrap_or(0)
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
        let (base_url, state) = start_fake_registry(bytes.clone(), digest.clone()).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let first = fetch_pack(&client, Some(home.path()), "gents", "plain_pack", None)
            .await
            .expect("first fetch");
        assert_eq!(first.namespace, "gents");
        assert_eq!(first.name, "plain_pack");
        assert_eq!(first.version, "1.0.0");
        assert_eq!(state.downloads.load(Ordering::SeqCst), 1);
        assert!(gents::pack_store::PackStore::new(home.path())
            .contains(&first.digest)
            .unwrap());

        // A second fetch of the same pack reads the cache: no second download.
        let second = fetch_pack(&client, Some(home.path()), "gents", "plain_pack", None)
            .await
            .expect("second fetch (cached)");
        assert_eq!(second.digest, first.digest);
        assert_eq!(
            state.downloads.load(Ordering::SeqCst),
            1,
            "second install must not refetch"
        );
    }

    #[tokio::test]
    async fn a_digest_mismatch_is_refused_naming_both_digests() {
        let (bytes, _real_digest) = sample_pack();
        let wrong_digest = "0".repeat(64);
        let (base_url, _state) = start_fake_registry(bytes, wrong_digest.clone()).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let error = fetch_pack(&client, Some(home.path()), "gents", "plain_pack", None)
            .await
            .expect_err("mismatched digest must be refused");
        let message = format!("{error:#}");
        assert!(message.contains(&wrong_digest), "{message}");
        assert!(message.contains("refusing to install"), "{message}");
        // Nothing was stored: a refusal is not a warning.
        assert_eq!(stored_packs(home.path()), 0);
    }

    #[tokio::test]
    async fn a_coordinate_mismatch_is_refused_before_the_pack_is_cached() {
        let (bytes, digest) = sample_pack();
        let (base_url, _state) = start_fake_registry(bytes, digest.clone()).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let error = fetch_pack(
            &client,
            Some(home.path()),
            "someone_else",
            "plain_pack",
            None,
        )
        .await
        .expect_err("a manifest from another namespace must be refused");
        assert!(
            format!("{error:#}").contains("different identity"),
            "{error:#}"
        );
        assert_eq!(stored_packs(home.path()), 0);
    }

    #[tokio::test]
    async fn an_unknown_pack_is_a_clean_not_found_not_a_silent_success() {
        let (bytes, digest) = sample_pack();
        let (base_url, _state) = start_fake_registry(bytes, digest).await;
        let client = RegistryClient::new(base_url);
        let home = tempfile::tempdir().unwrap();

        let error = fetch_pack(&client, Some(home.path()), "gents", "does_not_exist", None)
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
