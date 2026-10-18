//! Shared registry resolution for CLI and runtime pack consumers.
//!
//! Registry bytes are admitted once: the advertised artifact digest is
//! checked before parsing or caching, the archive validates its own bounded
//! contents, and the manifest must match the requested coordinate. Callers
//! may supply an explicit cache root; runtime callers that do not own a home
//! directory resolve in memory rather than guessing one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::pack::PackManifest;
use crate::pack_archive::PackArchive;
use crate::pack_store::PackStore;

pub const DEFAULT_REGISTRY_URL: &str = "https://registry.dev.gents.xyz";
pub const REGISTRY_ENV_VAR: &str = "GENTS_REGISTRY";

/// A registry failure a caller needs to react to, not just report: distinct
/// from a generic transport error so [`crate::pack_resolve::offline_pack_error`]
/// can classify it with [`anyhow::Error::chain`] and `downcast_ref`, rather
/// than pattern-matching on message text this module is free to reword.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error(
        "could not reach the registry at {url}; check the network, or choose another registry with --registry"
    )]
    Unreachable {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("the registry has nothing at {url}")]
    NotFound { url: String },
}

pub fn split_pack_coordinate(name: &str) -> (&str, &str) {
    name.split_once('/')
        .unwrap_or((crate::pack_archive::DEFAULT_NAMESPACE, name))
}

pub fn resolve_registry_url(explicit: Option<&str>) -> String {
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

/// A client for the packs registry. Everything it serves is a pack; a plugin
/// travels inside one.
pub struct RegistryClient {
    base_url: String,
    http: reqwest::Client,
}

impl RegistryClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::new(),
        }
    }

    fn unreachable(&self, source: reqwest::Error) -> RegistryError {
        RegistryError::Unreachable {
            url: self.base_url.clone(),
            source,
        }
    }

    /// `{base_url}/api/v1/<segments>`, one canonical builder for every
    /// registry path: each segment is percent-encoded through
    /// [`url::PathSegmentsMut::extend`], so a namespace, name or version
    /// containing a `/` or a space is encoded, never spliced into the path.
    fn api_url(&self, segments: &[&str]) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.base_url)
            .with_context(|| format!("{} is not a valid registry URL", self.base_url))?;
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("{} cannot be a base for a registry URL", self.base_url))?
            .extend(["api", "v1"])
            .extend(segments);
        Ok(url)
    }

    /// Whether `url`'s host is a loopback address or `localhost`: it never
    /// leaves the machine, so plain http there is not a cleartext leak.
    fn host_is_loopback(url: &reqwest::Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        if host.eq_ignore_ascii_case("localhost") {
            return true;
        }
        // `host_str` keeps the brackets around an IPv6 address.
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        host.parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    }

    /// Refuses a registry that is neither `https` nor loopback. Every call
    /// that sends a password or a bearer token goes through this first, so
    /// a mistyped or misconfigured `http://` registry cannot leak either in
    /// cleartext.
    fn require_secure(url: &reqwest::Url) -> Result<()> {
        anyhow::ensure!(
            url.scheme() == "https" || Self::host_is_loopback(url),
            "refusing to send a password or registry token to {url} over plain http; use an https:// registry, or one on localhost/127.0.0.1"
        );
        Ok(())
    }

    async fn json_or_error(response: reqwest::Response, url: &str) -> Result<Value> {
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(RegistryError::NotFound {
                url: url.to_owned(),
            }
            .into());
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

    async fn get_json(&self, segments: &[&str]) -> Result<Value> {
        let url = self.api_url(segments)?;
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|source| self.unreachable(source))?;
        Self::json_or_error(response, url.as_str()).await
    }

    pub async fn package(&self, namespace: &str, name: &str) -> Result<Value> {
        self.get_json(&["packs", namespace, name]).await
    }

    pub async fn version(&self, namespace: &str, name: &str, version: &str) -> Result<Value> {
        self.get_json(&["packs", namespace, name, version]).await
    }

    /// One page of search results; `page` is 1-based.
    pub async fn search(&self, query: &str, page: u32) -> Result<Value> {
        let url = self.api_url(&["packs"])?;
        let response = self
            .http
            .get(url.clone())
            .query(&[("q", query), ("page", &page.to_string())])
            .send()
            .await
            .map_err(|source| self.unreachable(source))?;
        Self::json_or_error(response, url.as_str()).await
    }

    pub async fn download(&self, namespace: &str, name: &str, version: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.download_into(namespace, name, version, &mut bytes)
            .await?;
        Ok(bytes)
    }

    /// Streams a version into `out` a chunk at a time, refusing it past the
    /// pack bound, and returns the sha256 hex of what was written.
    pub async fn download_into(
        &self,
        namespace: &str,
        name: &str,
        version: &str,
        out: &mut impl std::io::Write,
    ) -> Result<String> {
        use sha2::{Digest, Sha256};
        let url = self.api_url(&["packs", namespace, name, version, "download"])?;
        let mut response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|source| self.unreachable(source))?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "downloading {namespace}/{name}@{version} from the registry failed ({status})"
        );
        let limit = crate::pack_archive::MAX_PACK_BYTES as u64;
        if let Some(advertised) = response.content_length() {
            anyhow::ensure!(
                advertised <= limit,
                "registry pack {namespace}/{name}@{version} advertises {advertised} bytes, over the {limit} byte compressed bound"
            );
        }
        let mut hasher = Sha256::new();
        let mut written = 0u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("reading the download body from {url}"))?
        {
            written += chunk.len() as u64;
            anyhow::ensure!(
                written <= limit,
                "registry pack {namespace}/{name}@{version} exceeds the {limit} byte compressed bound"
            );
            hasher.update(&chunk);
            out.write_all(&chunk)
                .with_context(|| format!("saving {namespace}/{name}@{version}"))?;
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// Exchanges a username and password for an API token.
    pub async fn login(&self, username: &str, password: &str) -> Result<String> {
        let url = self.api_url(&["login"])?;
        Self::require_secure(&url)?;
        let response = self
            .http
            .post(url.clone())
            .json(&serde_json::json!({"username": username, "password": password}))
            .send()
            .await
            .map_err(|source| self.unreachable(source))?;
        Self::json_or_error(response, url.as_str())
            .await?
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("the registry answered a sign-in without a token")
    }

    /// Who `token` signs in as.
    pub async fn me(&self, token: &str) -> Result<Value> {
        let url = self.api_url(&["me"])?;
        Self::require_secure(&url)?;
        let response = self
            .http
            .get(url.clone())
            .bearer_auth(token)
            .send()
            .await
            .map_err(|source| self.unreachable(source))?;
        Self::json_or_error(response, url.as_str()).await
    }

    /// Gives a package to the account `username`; the caller must own it or
    /// be an admin.
    pub async fn transfer_owner(
        &self,
        token: &str,
        namespace: &str,
        name: &str,
        username: &str,
    ) -> Result<Value> {
        let url = self.api_url(&["packages", namespace, name, "owner"])?;
        Self::require_secure(&url)?;
        let response = self
            .http
            .post(url.clone())
            .bearer_auth(token)
            .json(&serde_json::json!({ "username": username }))
            .send()
            .await
            .map_err(|source| self.unreachable(source))?;
        Self::json_or_error(response, url.as_str()).await
    }

    /// Yanks a version, or restores it with `undo`.
    pub async fn yank(
        &self,
        token: &str,
        namespace: &str,
        name: &str,
        version: &str,
        undo: bool,
    ) -> Result<Value> {
        let url = self.api_url(&["packs", namespace, name, version, "yank"])?;
        Self::require_secure(&url)?;
        let response = self
            .http
            .post(url.clone())
            .query(&[("undo", undo)])
            .bearer_auth(token)
            .send()
            .await
            .map_err(|source| self.unreachable(source))?;
        Self::json_or_error(response, url.as_str()).await
    }

    pub async fn publish(&self, token: &str, bytes: Vec<u8>) -> Result<Value> {
        let url = self.api_url(&["publish"])?;
        Self::require_secure(&url)?;
        let response = self
            .http
            .post(url.clone())
            .bearer_auth(token)
            .body(bytes)
            .send()
            .await
            .with_context(|| format!("publishing to {url}"))?;
        Self::json_or_error(response, url.as_str()).await
    }
}

#[derive(Debug)]
pub struct RegistryPack {
    pub archive: PackArchive,
    /// Canonical digest over declared pack assets, shared with every pack loader.
    pub digest: String,
    /// Registry artifact digest over the exact compressed archive bytes.
    pub artifact_digest: String,
    pub namespace: String,
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryCoordinate {
    pub namespace: String,
    pub name: String,
    pub version: String,
    pub artifact_digest: String,
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

pub fn verify_digest(bytes: &[u8], advertised: &str, coordinate: &str) -> Result<()> {
    let computed = sha256_hex(bytes);
    anyhow::ensure!(
        computed == advertised,
        "the registry advertised digest {advertised} for {coordinate} but the bytes hash to {computed}; refusing to install a pack that does not match what the registry described"
    );
    Ok(())
}

pub fn verify_pack_coordinate(
    manifest: &PackManifest,
    namespace: &str,
    name: &str,
    version: &str,
) -> Result<()> {
    anyhow::ensure!(
        manifest.metadata.namespace == namespace
            && manifest.name == name
            && manifest.version == version,
        "the registry returned pack {}/{name_in_manifest}@{version_in_manifest} for requested coordinate {namespace}/{name}@{version}; refusing to install an artifact under a different identity",
        manifest.metadata.namespace,
        name_in_manifest = manifest.name,
        version_in_manifest = manifest.version,
    );
    Ok(())
}

pub async fn resolve_pack_coordinate(
    client: &RegistryClient,
    namespace: &str,
    name: &str,
    version: Option<&str>,
) -> Result<RegistryCoordinate> {
    let version = match version {
        Some(version) => version.to_owned(),
        None => client
            .package(namespace, name)
            .await
            .with_context(|| format!("looking up {namespace}/{name} on the registry"))?
            .get("latest")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("registry package has no published version")?
            .to_owned(),
    };
    let artifact_digest = client
        .version(namespace, name, &version)
        .await?
        .get("digest")
        .and_then(Value::as_str)
        .context("the registry did not advertise an artifact digest")?
        .to_owned();
    Ok(RegistryCoordinate {
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        version,
        artifact_digest,
    })
}

pub async fn download_verified_pack(
    client: &RegistryClient,
    coordinate: &RegistryCoordinate,
) -> Result<Vec<u8>> {
    let bytes = client
        .download(&coordinate.namespace, &coordinate.name, &coordinate.version)
        .await?;
    verify_digest(
        &bytes,
        &coordinate.artifact_digest,
        &format!(
            "{}/{}@{}",
            coordinate.namespace, coordinate.name, coordinate.version
        ),
    )?;
    Ok(bytes)
}

/// Resolve the latest registry coordinate and fetch that pack. With a home,
/// the verified download goes into its [`PackStore`] and a later fetch of the
/// same version reads the store instead of downloading again. `None` performs
/// the same admission entirely in memory for runtime consumers without a home.
pub async fn fetch_pack(
    client: &RegistryClient,
    cache_home: Option<&Path>,
    namespace: &str,
    name: &str,
    version: Option<&str>,
) -> Result<RegistryPack> {
    let coordinate = resolve_pack_coordinate(client, namespace, name, version).await?;
    let coordinate_label = format!(
        "{}/{}@{}",
        coordinate.namespace, coordinate.name, coordinate.version
    );
    // The registry names a download by the digest of its bytes, so this
    // index maps that to the pack digest the store uses; delete it once the
    // registry advertises pack digests directly.
    let advertised = format!("sha256:{}", coordinate.artifact_digest);
    let index = cache_home
        .map(|home| -> Result<PathBuf> {
            let hex = crate::pack_archive::digest_hex(&advertised)
                .context("the registry advertised a malformed digest")?;
            Ok(home
                .join(crate::home::PACKS_DIR_NAME)
                .join("store")
                .join("by-download")
                .join(hex))
        })
        .transpose()?;
    let store = cache_home.map(PackStore::new);
    let cached = match (&store, &index) {
        (Some(store), Some(index)) if index.is_file() => {
            let digest = std::fs::read_to_string(index)
                .with_context(|| format!("reading {}", index.display()))?;
            let digest = digest.trim();
            if store.contains(digest)? {
                let archive = store.open(digest)?;
                // A by-download hit never goes through `import_accepting`,
                // which is where `PackStore::index` is normally called: index
                // it here too, so a pack already in the store from an
                // earlier fetch becomes resolvable by name without another
                // network round trip.
                store.index(archive.header())?;
                Some(archive)
            } else {
                // The digest this entry named was released from the store
                // since it was cached (`gents pack remove`, then a reinstall
                // of the same version): treat it as a miss so the download
                // below refreshes the mapping instead of failing on a digest
                // that is simply gone.
                None
            }
        }
        _ => None,
    };
    let archive = match (cached, &store, &index, cache_home) {
        (Some(archive), _, _, _) => archive,
        // With a home, the download streams to disk and into the store, and
        // the pack is opened from there: no step holds it in memory.
        (None, Some(store), Some(index), Some(home)) => {
            let staging_dir = home.join(crate::home::PACKS_DIR_NAME);
            std::fs::create_dir_all(&staging_dir)
                .with_context(|| format!("creating {}", staging_dir.display()))?;
            let mut staged = tempfile::NamedTempFile::new_in(&staging_dir)
                .with_context(|| format!("staging a download in {}", staging_dir.display()))?;
            let computed = {
                let mut writer = std::io::BufWriter::new(staged.as_file_mut());
                let computed = client
                    .download_into(
                        &coordinate.namespace,
                        &coordinate.name,
                        &coordinate.version,
                        &mut writer,
                    )
                    .await?;
                std::io::Write::flush(&mut writer).context("saving the download")?;
                computed
            };
            anyhow::ensure!(
                computed == coordinate.artifact_digest,
                "the registry advertised digest {} for {coordinate_label} but the bytes hash to {computed}; refusing to install a pack that does not match what the registry described",
                coordinate.artifact_digest
            );
            let stored = store
                .import_file_accepting(staged.path(), None, |verified| {
                    verify_pack_coordinate(
                        &verified.manifest,
                        &coordinate.namespace,
                        &coordinate.name,
                        &coordinate.version,
                    )
                })
                .with_context(|| format!("installing {coordinate_label} from the registry"))?;
            let archive = store.open(&stored.header.digest)?;
            let dir = index.parent().context("download index has no parent")?;
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
            // `stage_and_persist` will not overwrite an existing file; drop a
            // stale entry first (one pointing at a since-released digest) so
            // the fresh mapping actually lands instead of silently keeping
            // the old one.
            if let Err(error) = std::fs::remove_file(index) {
                anyhow::ensure!(
                    error.kind() == std::io::ErrorKind::NotFound,
                    "removing the stale download index at {}: {error}",
                    index.display()
                );
            }
            stage_and_persist(dir, index, stored.header.digest.as_bytes())?;
            archive
        }
        _ => {
            let bytes = download_verified_pack(client, &coordinate).await?;
            PackArchive::from_bytes(&bytes).with_context(|| {
                format!("{coordinate_label} from the registry is not a readable pack")
            })?
        }
    };
    verify_pack_coordinate(
        archive.manifest(),
        &coordinate.namespace,
        &coordinate.name,
        &coordinate.version,
    )?;
    Ok(RegistryPack {
        digest: archive.digest().to_owned(),
        archive,
        artifact_digest: coordinate.artifact_digest,
        namespace: coordinate.namespace,
        name: coordinate.name,
        version: coordinate.version,
    })
}

/// Registry tokens saved by `gents pack login`, one per registry URL, in
/// `{home}/registry/credentials.json`, readable only by its owner.
pub mod credentials {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use anyhow::{Context, Result};

    fn path(home: &Path) -> PathBuf {
        home.join(crate::home::REGISTRY_DIR_NAME)
            .join("credentials.json")
    }

    fn lock_path(home: &Path) -> PathBuf {
        home.join(crate::home::REGISTRY_DIR_NAME)
            .join("credentials.json.lock")
    }

    /// An exclusive lock held across a read-modify-write of the credentials
    /// file, so two `gents pack login`/`logout` invocations racing on the
    /// same home cannot interleave and drop one write.
    fn exclusive_lock(home: &Path) -> Result<std::fs::File> {
        let path = lock_path(home);
        let dir = path
            .parent()
            .context("credentials lock path has no parent")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.lock()
            .with_context(|| format!("locking {}", path.display()))?;
        Ok(file)
    }

    fn read_all(home: &Path) -> Result<BTreeMap<String, String>> {
        match std::fs::read(path(home)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not valid JSON", path(home).display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path(home).display())),
        }
    }

    fn write_all(home: &Path, tokens: &BTreeMap<String, String>) -> Result<()> {
        let file = path(home);
        let dir = file.parent().context("credentials path has no parent")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let mut staged = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("staging credentials in {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            staged
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .context("restricting the credentials file")?;
        }
        std::io::Write::write_all(&mut staged, &serde_json::to_vec_pretty(tokens)?)
            .context("writing credentials")?;
        staged
            .persist(&file)
            .map_err(|error| error.error)
            .with_context(|| format!("saving {}", file.display()))?;
        Ok(())
    }

    /// The token saved for `registry`, if any.
    pub fn get(home: &Path, registry: &str) -> Result<Option<String>> {
        Ok(read_all(home)?.remove(registry.trim_end_matches('/')))
    }

    pub fn set(home: &Path, registry: &str, token: &str) -> Result<()> {
        let _lock = exclusive_lock(home)?;
        let mut tokens = read_all(home)?;
        tokens.insert(registry.trim_end_matches('/').to_owned(), token.to_owned());
        write_all(home, &tokens)
    }

    /// Forgets the token for `registry`; returns whether there was one.
    pub fn remove(home: &Path, registry: &str) -> Result<bool> {
        let _lock = exclusive_lock(home)?;
        let mut tokens = read_all(home)?;
        let removed = tokens.remove(registry.trim_end_matches('/')).is_some();
        if removed {
            write_all(home, &tokens)?;
        }
        Ok(removed)
    }
}

pub fn stage_and_persist(dir: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
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
        Err(error) => Err(error.error).with_context(|| format!("saving {}", dest.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal fake registry serving exactly one namespace/name/version,
    /// for [`fetch_pack`] tests: `package` and `version` answer with just
    /// enough to resolve, and `download` streams `bytes` back verbatim.
    async fn serve_one_pack(
        namespace: &str,
        name: &str,
        version: &str,
        bytes: Vec<u8>,
        artifact_digest: String,
    ) -> String {
        let package_path = format!("/api/v1/packs/{namespace}/{name}");
        let version_path = format!("/api/v1/packs/{namespace}/{name}/{version}");
        let download_path = format!("{version_path}/download");
        let latest = version.to_owned();
        let app = axum::Router::new()
            .route(
                &package_path,
                axum::routing::get(move || {
                    let latest = latest.clone();
                    async move { axum::Json(serde_json::json!({ "latest": latest })) }
                }),
            )
            .route(
                &version_path,
                axum::routing::get(move || {
                    let digest = artifact_digest.clone();
                    async move { axum::Json(serde_json::json!({ "digest": digest })) }
                }),
            )
            .route(
                &download_path,
                axum::routing::get(move || {
                    let bytes = bytes.clone();
                    async move { bytes }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// A by-download cache hit skips [`PackStore::import_accepting`], the
    /// usual place a header is indexed by name; it must index the pack
    /// itself, or a pack already in the store from an earlier fetch stays
    /// unresolvable by name forever (D1's store-first, zero-network promise
    /// broken on exactly the packs it exists to speed up).
    #[tokio::test]
    async fn a_by_download_cache_hit_fills_in_a_missing_name_index_entry() {
        let (bytes, header) =
            crate::pack_store::test_pack_named("registry_cache_hit_fixture", "1.0.0");
        let artifact_digest = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(&bytes))
        };
        let registry_url = serve_one_pack(
            "gents",
            "registry_cache_hit_fixture",
            "1.0.0",
            bytes,
            artifact_digest,
        )
        .await;
        let client = RegistryClient::new(registry_url);
        let home = tempfile::tempdir().unwrap();

        fetch_pack(
            &client,
            Some(home.path()),
            "gents",
            "registry_cache_hit_fixture",
            None,
        )
        .await
        .expect("the first, uncached fetch");
        let store = PackStore::new(home.path());
        assert_eq!(
            store
                .lookup("gents", "registry_cache_hit_fixture", None)
                .unwrap()
                .map(|found| found.digest),
            Some(header.digest.clone()),
            "the uncached fetch indexes the pack through `import_accepting`"
        );

        // Simulate a pack the store already held before it ever had a name
        // index entry: the by-download cache stays untouched, but the name
        // index has nothing for it.
        let index_entry = home
            .path()
            .join(crate::home::PACKS_DIR_NAME)
            .join("store")
            .join("by-name")
            .join("gents")
            .join("registry_cache_hit_fixture")
            .join("1.0.0");
        std::fs::remove_file(&index_entry).unwrap();
        assert_eq!(
            store
                .lookup("gents", "registry_cache_hit_fixture", None)
                .unwrap(),
            None
        );

        fetch_pack(
            &client,
            Some(home.path()),
            "gents",
            "registry_cache_hit_fixture",
            None,
        )
        .await
        .expect("a by-download cache hit");
        assert_eq!(
            store
                .lookup("gents", "registry_cache_hit_fixture", None)
                .unwrap()
                .map(|found| found.digest),
            Some(header.digest),
            "the cache-hit fetch backfills the name index"
        );
    }

    /// After `gents pack remove` releases a digest, its `by-download` entry
    /// still names it. The next `fetch_pack` of the same version must not
    /// fail with "is not in the store"; it must treat the stale entry as a
    /// miss and download again.
    #[tokio::test]
    async fn a_stale_by_download_entry_after_release_is_treated_as_a_miss() {
        let (bytes, header) = crate::pack_store::test_pack_named("registry_stale_fixture", "1.0.0");
        let artifact_digest = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(&bytes))
        };
        let registry_url = serve_one_pack(
            "gents",
            "registry_stale_fixture",
            "1.0.0",
            bytes,
            artifact_digest,
        )
        .await;
        let client = RegistryClient::new(registry_url);
        let home = tempfile::tempdir().unwrap();

        fetch_pack(
            &client,
            Some(home.path()),
            "gents",
            "registry_stale_fixture",
            None,
        )
        .await
        .expect("the first fetch");
        let store = PackStore::new(home.path());
        assert!(store.contains(&header.digest).unwrap());

        store.release(&header.digest).unwrap();
        assert!(!store.contains(&header.digest).unwrap());

        let fetched = fetch_pack(
            &client,
            Some(home.path()),
            "gents",
            "registry_stale_fixture",
            None,
        )
        .await
        .expect("a stale by-download entry must not fail the fetch");
        assert_eq!(fetched.digest, header.digest);
        assert!(store.contains(&header.digest).unwrap());
    }

    #[tokio::test]
    async fn an_unreachable_registry_says_what_to_do_in_gents_terms() {
        let client = RegistryClient::new("http://127.0.0.1:1".to_string());
        for error in [
            client.package("acme", "demo").await.unwrap_err(),
            client.search("demo", 1).await.unwrap_err(),
            client.download("acme", "demo", "1.0.0").await.unwrap_err(),
        ] {
            let message = format!("{error:#}");
            assert!(
                message.contains("could not reach the registry at http://127.0.0.1:1"),
                "{message}"
            );
            assert!(message.contains("--registry"), "{message}");
            assert!(!message.contains("curl"), "{message}");
        }
    }

    #[test]
    fn credentials_are_saved_per_registry_privately_and_forgotten() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            credentials::get(home.path(), "https://a.example").unwrap(),
            None
        );
        credentials::set(home.path(), "https://a.example/", "gcpat_a").unwrap();
        credentials::set(home.path(), "https://b.example", "gcpat_b").unwrap();
        assert_eq!(
            credentials::get(home.path(), "https://a.example")
                .unwrap()
                .as_deref(),
            Some("gcpat_a")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.path().join("registry/credentials.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(credentials::remove(home.path(), "https://a.example").unwrap());
        assert!(!credentials::remove(home.path(), "https://a.example").unwrap());
        assert_eq!(
            credentials::get(home.path(), "https://b.example")
                .unwrap()
                .as_deref(),
            Some("gcpat_b")
        );
    }

    #[test]
    fn a_name_with_a_space_or_slash_is_encoded_not_injected() {
        let client = RegistryClient::new("https://registry.example".to_string());
        let url = client
            .api_url(&["packs", "acme", "weird name/with-slash", "1.0.0"])
            .unwrap();
        // A literal `/` in a segment must not open a new path component, and
        // a space must not reach the wire unescaped.
        assert_eq!(
            url.as_str(),
            "https://registry.example/api/v1/packs/acme/weird%20name%2Fwith-slash/1.0.0"
        );
        assert_eq!(url.path_segments().unwrap().count(), 6);
    }

    #[test]
    fn only_https_or_loopback_registries_may_carry_a_password_or_token() {
        let secure = reqwest::Url::parse("https://registry.example/api/v1/login").unwrap();
        RegistryClient::require_secure(&secure).expect("https is always allowed");

        for loopback in [
            "http://127.0.0.1:8080/api/v1/login",
            "http://[::1]:8080/api/v1/login",
            "http://localhost:8080/api/v1/login",
        ] {
            let url = reqwest::Url::parse(loopback).unwrap();
            RegistryClient::require_secure(&url)
                .unwrap_or_else(|error| panic!("{loopback} must be allowed: {error:#}"));
        }

        let cleartext = reqwest::Url::parse("http://registry.example/api/v1/login").unwrap();
        let error = RegistryClient::require_secure(&cleartext).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("https"), "{message}");
        assert!(message.contains("http://registry.example"), "{message}");
    }
}
