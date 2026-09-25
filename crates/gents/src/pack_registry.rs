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

pub const DEFAULT_REGISTRY_URL: &str = "https://packs.gents.xyz";
pub const REGISTRY_ENV_VAR: &str = "GENTS_REGISTRY";

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

    fn unreachable(&self) -> String {
        format!(
            "could not reach the registry at {}; check the network, or choose another registry with --registry",
            self.base_url
        )
    }

    /// An endpoint that is the same for packs and plugins.
    fn base_api(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.base_url)
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
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::json_or_error(response, &url).await
    }

    pub async fn package(&self, namespace: &str, name: &str) -> Result<Value> {
        self.get_json(&format!("/packs/{namespace}/{name}")).await
    }

    pub async fn version(&self, namespace: &str, name: &str, version: &str) -> Result<Value> {
        self.get_json(&format!("/packs/{namespace}/{name}/{version}"))
            .await
    }

    /// One page of search results; `page` is 1-based.
    pub async fn search(&self, query: &str, page: u32) -> Result<Value> {
        let url = self.api("/packs");
        let response = self
            .http
            .get(&url)
            .query(&[("q", query), ("page", &page.to_string())])
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::json_or_error(response, &url).await
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
        let url = self.api(&format!("/packs/{namespace}/{name}/{version}/download"));
        let mut response = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| self.unreachable())?;
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
        let url = self.base_api("/login");
        let response = self
            .http
            .post(&url)
            .json(&serde_json::json!({"username": username, "password": password}))
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::json_or_error(response, &url)
            .await?
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("the registry answered a sign-in without a token")
    }

    /// Who `token` signs in as.
    pub async fn me(&self, token: &str) -> Result<Value> {
        let url = self.base_api("/me");
        let response = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::json_or_error(response, &url).await
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
        let url = self.base_api(&format!("/packages/{namespace}/{name}/owner"));
        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "username": username }))
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::json_or_error(response, &url).await
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
        let url = self.api(&format!("/packs/{namespace}/{name}/{version}/yank"));
        let response = self
            .http
            .post(&url)
            .query(&[("undo", undo)])
            .bearer_auth(token)
            .send()
            .await
            .with_context(|| self.unreachable())?;
        Self::json_or_error(response, &url).await
    }

    pub async fn publish(&self, token: &str, bytes: Vec<u8>) -> Result<Value> {
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

#[derive(Debug)]
pub struct RegistryPack {
    pub archive: PackArchive,
    /// Canonical digest over declared pack assets, shared with bundled packs.
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
    // vertexia: the registry names a download by the digest of its bytes, so
    // this index maps that to the pack digest the store uses; delete it once
    // the registry advertises pack digests.
    let advertised = format!("sha256:{}", coordinate.artifact_digest);
    let index = cache_home
        .map(|home| -> Result<PathBuf> {
            let hex = crate::pack_archive::digest_hex(&advertised)
                .context("the registry advertised a malformed digest")?;
            Ok(home
                .join("packs")
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
            Some(store.open(digest.trim())?)
        }
        _ => None,
    };
    let archive = match (cached, &store, &index, cache_home) {
        (Some(archive), _, _, _) => archive,
        // With a home, the download streams to disk and into the store, and
        // the pack is opened from there: no step holds it in memory.
        (None, Some(store), Some(index), Some(home)) => {
            let staging_dir = home.join("packs");
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
        home.join("registry").join("credentials.json")
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
        let mut tokens = read_all(home)?;
        tokens.insert(registry.trim_end_matches('/').to_owned(), token.to_owned());
        write_all(home, &tokens)
    }

    /// Forgets the token for `registry`; returns whether there was one.
    pub fn remove(home: &Path, registry: &str) -> Result<bool> {
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
}
