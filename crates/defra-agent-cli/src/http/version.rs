use serde::{Deserialize, Serialize};

const SERVICE_NAME: &str = "defra-agent";
const SERVICE_BINARY: &str = "defra-agent";

#[derive(Debug, Clone, Serialize)]
pub(crate) struct VersionResponse {
    pub(super) service: &'static str,
    pub(super) binary: &'static str,
    pub(super) package: &'static str,
    pub(super) version: &'static str,
    pub(super) repository: &'static str,
    pub(super) build: BuildMetadata,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct BuildMetadata {
    pub(super) git_sha: Option<&'static str>,
    pub(super) git_ref: Option<&'static str>,
    pub(super) git_dirty: Option<bool>,
    pub(super) target: Option<&'static str>,
    pub(super) profile: Option<&'static str>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NodeIdentityResponse {
    #[serde(rename = "PeerID")]
    pub(crate) peer_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct P2pShareableAddressResponse {
    #[serde(default)]
    pub(crate) address: Option<String>,
}

pub(crate) fn version_response() -> VersionResponse {
    VersionResponse {
        service: SERVICE_NAME,
        binary: SERVICE_BINARY,
        package: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        repository: env!("CARGO_PKG_REPOSITORY"),
        build: BuildMetadata {
            git_sha: option_env!("DEFRA_AGENT_BUILD_GIT_SHA"),
            git_ref: option_env!("DEFRA_AGENT_BUILD_GIT_REF"),
            git_dirty: option_env!("DEFRA_AGENT_BUILD_GIT_DIRTY").and_then(|value| match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }),
            target: option_env!("DEFRA_AGENT_BUILD_TARGET"),
            profile: option_env!("DEFRA_AGENT_BUILD_PROFILE"),
        },
    }
}
