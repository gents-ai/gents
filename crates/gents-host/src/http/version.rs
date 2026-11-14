use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct VersionResponse {
    pub service: &'static str,
    pub binary: &'static str,
    pub package: &'static str,
    pub version: &'static str,
    pub repository: &'static str,
    pub build: BuildMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildMetadata {
    pub git_sha: Option<&'static str>,
    pub git_ref: Option<&'static str>,
    pub git_tag: Option<&'static str>,
    pub git_dirty: Option<bool>,
    pub target: Option<&'static str>,
    pub profile: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct NodeIdentityResponse {
    #[serde(rename = "PeerID")]
    pub peer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct P2pShareableAddressResponse {
    #[serde(default)]
    pub address: Option<String>,
}

static VERSION: std::sync::OnceLock<VersionResponse> = std::sync::OnceLock::new();

pub fn install_version(version: VersionResponse) {
    let _ = VERSION.set(version);
}

pub fn version_response() -> VersionResponse {
    VERSION.get().cloned().unwrap_or(VersionResponse {
        service: "gents",
        binary: "gents",
        package: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        repository: env!("CARGO_PKG_REPOSITORY"),
        build: BuildMetadata {
            git_sha: None,
            git_ref: None,
            git_tag: None,
            git_dirty: None,
            target: None,
            profile: None,
        },
    })
}
