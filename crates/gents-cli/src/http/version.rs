use serde::{Deserialize, Serialize};

const SERVICE_NAME: &str = "gents";
const SERVICE_BINARY: &str = "gents";

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
    pub(super) git_tag: Option<&'static str>,
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
            git_sha: option_env!("GENTS_BUILD_GIT_SHA"),
            git_ref: option_env!("GENTS_BUILD_GIT_REF"),
            git_tag: option_env!("GENTS_BUILD_GIT_TAG"),
            git_dirty: option_env!("GENTS_BUILD_GIT_DIRTY").and_then(|value| match value {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            }),
            target: option_env!("GENTS_BUILD_TARGET"),
            profile: option_env!("GENTS_BUILD_PROFILE"),
        },
    }
}

pub(crate) fn version_text() -> String {
    let version = version_response();
    let mut revision = version.build.git_sha.unwrap_or("unknown").to_string();
    if version.build.git_dirty == Some(true) {
        revision.push_str("-dirty");
    }
    if let Some(tag) = version.build.git_tag {
        revision.push_str(", tag ");
        revision.push_str(tag);
    }

    format!(
        "{} {} ({revision})\nbuilt {} for {}\n{}\n",
        version.binary,
        version.version,
        version.build.profile.unwrap_or("unknown"),
        version.build.target.unwrap_or("unknown"),
        option_env!("GENTS_BUILD_RUSTC").unwrap_or("rustc unknown")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_response_reports_canonical_repository() {
        assert_eq!(
            version_response().repository,
            "https://github.com/source-inc/gents"
        );
    }
}
