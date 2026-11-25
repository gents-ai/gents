use super::*;
use crate::native_service::CommandRunner;
use gents_protocol::error_chain::PassedThrough;
use gents_store::local::store_key::{
    StoreEncryption, StoreKey, StoreKeyCustody, StoreKeyError, StoreKeyUnavailable,
    MACOS_KEYCHAIN_SERVICE,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum PreflightError {
    #[error("candidate preflight could not complete; runtime and service unchanged")]
    Observation(#[source] PassedThrough),
    #[error("candidate preflight returned invalid metadata; runtime and service unchanged")]
    Metadata(#[from] serde_json::Error),
    #[error("this build cannot read the store key of {} without interaction (Keychain service `{service}`, account `{account}`); runtime and service unchanged. A person at the Mac must approve this build's access to that item (and unlock the login Keychain if needed), or install a build signed with the same stable identity as the running one; then retry your update command", home.display())]
    StoreKey {
        home: PathBuf,
        service: &'static str,
        account: String,
        #[source]
        source: StoreKeyUnavailable,
    },
    #[error(transparent)]
    StoreKeyRead(#[from] Box<StoreKeyError>),
    #[error("candidate preflight failed; runtime and service unchanged. Fix the cause, then retry your update command. Diagnostic: {detail}")]
    CandidateRefused { detail: String },
    #[error("candidate platform, schema check or handoff protocol mismatch; runtime and service unchanged")]
    Incompatible,
}

fn observation(error: anyhow::Error) -> PreflightError {
    PreflightError::Observation(PassedThrough::new(error))
}

pub(super) fn keychain_custody(encryption: Option<&StoreEncryption>) -> bool {
    encryption.is_some_and(|record| matches!(record.custody, StoreKeyCustody::MacosKeychain { .. }))
}

pub(super) fn check_store_key(
    home: &Path,
    encryption: Option<&StoreEncryption>,
) -> std::result::Result<(), PreflightError> {
    check_store_key_with(home, encryption, StoreEncryption::load)
}

pub(crate) fn check_store_key_with(
    home: &Path,
    encryption: Option<&StoreEncryption>,
    read: impl FnOnce(&StoreEncryption, &Path, &Path) -> std::result::Result<StoreKey, StoreKeyError>,
) -> std::result::Result<(), PreflightError> {
    let Some(
        record @ StoreEncryption {
            custody: StoreKeyCustody::MacosKeychain { keychain_label },
            ..
        },
    ) = encryption
    else {
        return Ok(());
    };
    let _no_prompts =
        crate::process_resources::prevent_keychain_prompts_guard().map_err(observation)?;
    read(
        record,
        &gents_store::local::store_key::home_key_file(home),
        &gents_store::local::home::default_data_dir(home),
    )
    .map(drop)
    .map_err(|source| match source {
        StoreKeyError::Unavailable(source) => PreflightError::StoreKey {
            home: home.to_owned(),
            service: MACOS_KEYCHAIN_SERVICE,
            account: keychain_label.clone(),
            source,
        },
        other => PreflightError::StoreKeyRead(Box::new(other)),
    })
}

pub(super) fn probe(
    runner: &impl CommandRunner,
    candidate: &Path,
    home: &Path,
    keychain: bool,
) -> std::result::Result<Phase, PreflightError> {
    let output = runner
        .run(
            candidate.as_os_str(),
            &[
                "service".into(),
                "update-check".into(),
                "--home".into(),
                home.as_os_str().to_owned(),
            ],
        )
        .map_err(observation)?;
    let verified = output.success && {
        let metadata: serde_json::Value = serde_json::from_str(&output.stdout)?;
        metadata["handoff_version"] == 1
            && metadata["schema_checked"] == true
            && metadata["os"] == std::env::consts::OS
            && metadata["arch"] == std::env::consts::ARCH
    };
    gents_protocol::serve_lifecycle::update::preflight(verified, keychain, output.success)
        .ok_or_else(|| match output.success {
            true => PreflightError::Incompatible,
            false => PreflightError::CandidateRefused {
                detail: gents_protocol::failure_text::bounded_text(&output.stderr),
            },
        })
}

#[cfg(test)]
mod tests;
