use gents_host::http::version::{BuildMetadata, VersionResponse};

const SERVICE_NAME: &str = "gents";
const SERVICE_BINARY: &str = "gents";

pub(crate) fn build_version() -> VersionResponse {
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
    let version = build_version();
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
