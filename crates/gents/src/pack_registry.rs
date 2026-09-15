//! Shared registry resolution for CLI and runtime pack consumers.
//!
//! Registry bytes are admitted once: the advertised artifact digest is
//! checked before parsing or caching, the archive validates its own bounded
//! contents, and the manifest must match the requested coordinate. Callers
//! may supply an explicit cache root; runtime callers that do not own a home
//! directory resolve in memory rather than guessing one.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::pack::PackManifest;
use crate::pack_archive::PackArchive;

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

pub struct RegistryClient {
    base_url: String,
    kind: RegistryKind,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryKind {
    Pack,
    Plugin,
}

impl RegistryKind {
    pub fn path(self) -> &'static str {
        match self {
            Self::Pack => "packs",
            Self::Plugin => "packages",
        }
    }
}

impl RegistryClient {
    pub fn new(base_url: String) -> Self {
        Self::for_kind(base_url, RegistryKind::Pack)
    }

    pub fn for_plugins(base_url: String) -> Self {
        Self::for_kind(base_url, RegistryKind::Plugin)
    }

    fn for_kind(base_url: String, kind: RegistryKind) -> Self {
        Self {
            base_url,
            kind,
            http: reqwest::Client::new(),
        }
    }

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
                "requesting {url}; if this machine cannot reach the registry, the same request by hand is: {}",
                Self::curl_equivalent(&url, false)
            )
        })?;
        Self::json_or_error(response, &url).await
    }

    pub async fn package(&self, namespace: &str, name: &str) -> Result<Value> {
        self.get_json(&format!("/{}/{namespace}/{name}", self.kind.path()))
            .await
    }

    pub async fn version(&self, namespace: &str, name: &str, version: &str) -> Result<Value> {
        self.get_json(&format!(
            "/{}/{namespace}/{name}/{version}",
            self.kind.path()
        ))
        .await
    }

    pub async fn search(&self, query: &str) -> Result<Value> {
        let url = self.api(&format!("/{}", self.kind.path()));
        let response = self
            .http
            .get(&url)
            .query(&[("q", query)])
            .send()
            .await
            .with_context(|| {
                format!(
                    "requesting {url}; if this machine cannot reach the registry, the same request by hand is: {}",
                    Self::curl_equivalent(&url, false)
                )
            })?;
        Self::json_or_error(response, &url).await
    }

    pub async fn download(&self, namespace: &str, name: &str, version: &str) -> Result<Vec<u8>> {
        let url = self.api(&format!(
            "/{}/{namespace}/{name}/{version}/download",
            self.kind.path()
        ));
        let mut response = self.http.get(&url).send().await.with_context(|| {
            format!(
                "downloading {url}; if this machine cannot reach the registry, fetch it by hand with `{} -o {name}-{version}.tar.gz` and install that file",
                Self::curl_equivalent(&url, false)
            )
        })?;
        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "downloading {namespace}/{name}@{version} from the registry failed ({status})"
        );
        let limit = crate::pack_archive::MAX_PACK_BYTES;
        if let Some(advertised) = response.content_length() {
            anyhow::ensure!(
                advertised <= limit as u64,
                "registry pack {namespace}/{name}@{version} advertises {advertised} bytes, over the {limit} byte compressed bound"
            );
        }
        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .unwrap_or_default()
                .min(limit as u64) as usize,
        );
        while let Some(chunk) = response
            .chunk()
            .await
            .with_context(|| format!("reading the download body from {url}"))?
        {
            anyhow::ensure!(
                bytes.len().saturating_add(chunk.len()) <= limit,
                "registry pack {namespace}/{name}@{version} exceeds the {limit} byte compressed bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
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

/// Resolve the latest registry coordinate. An explicit cache root preserves
/// the CLI's verified content-addressed cache. `None` performs the same
/// admission entirely in memory for runtime consumers without a home owner.
pub async fn fetch_pack(
    client: &RegistryClient,
    cache_home: Option<&Path>,
    namespace: &str,
    name: &str,
) -> Result<RegistryPack> {
    let coordinate = resolve_pack_coordinate(client, namespace, name, None).await?;
    let advertised = &coordinate.artifact_digest;
    let coordinate_label = format!(
        "{}/{}@{}",
        coordinate.namespace, coordinate.name, coordinate.version
    );
    let cache_path = cache_home.map(|home| {
        home.join("packs")
            .join("registry-cache")
            .join(format!("{advertised}.tar.gz"))
    });
    let cache_hit = cache_path.as_ref().is_some_and(|path| path.is_file());
    let bytes = if let Some(path) = cache_path.as_ref().filter(|_| cache_hit) {
        let cached = std::fs::read(path)
            .with_context(|| format!("reading the cached pack {}", path.display()))?;
        verify_digest(&cached, advertised, &coordinate_label)?;
        cached
    } else {
        download_verified_pack(client, &coordinate).await?
    };
    let archive = PackArchive::from_bytes(&bytes)
        .with_context(|| format!("{coordinate_label} from the registry is not a readable pack"))?;
    verify_pack_coordinate(
        archive.manifest(),
        &coordinate.namespace,
        &coordinate.name,
        &coordinate.version,
    )?;
    if !cache_hit {
        if let Some(path) = &cache_path {
            let dir = path.parent().context("registry cache path has no parent")?;
            std::fs::create_dir_all(dir).with_context(|| {
                format!("creating the pack download cache under {}", dir.display())
            })?;
            stage_and_persist(dir, path, &bytes)?;
        }
    }
    let digest = archive
        .digest()
        .with_context(|| format!("{coordinate_label} failed its canonical content check"))?;
    Ok(RegistryPack {
        archive,
        digest,
        artifact_digest: coordinate.artifact_digest,
        namespace: coordinate.namespace,
        name: coordinate.name,
        version: coordinate.version,
    })
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
        Err(error) => Err(error.error)
            .with_context(|| format!("saving the downloaded pack to {}", dest.display())),
    }
}
