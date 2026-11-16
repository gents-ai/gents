use super::*;
use crate::native_service::{CommandRunner, ProcessCommandRunner};
use gents_store::local::store_key::{StoreEncryption, StoreKey, StoreKeyCustody, StoreKeyError};

#[derive(Debug, thiserror::Error)]
pub(super) enum PreflightError {
    #[error("candidate preflight could not complete; runtime and service unchanged")]
    Observation(#[source] gents_protocol::error_chain::PassedThrough),
    #[error("candidate preflight returned invalid metadata; runtime and service unchanged")]
    Metadata(#[from] serde_json::Error),
    #[error("candidate cannot read this home's store key without a Keychain prompt: {}; this build may not be trusted by the Keychain item (or the login Keychain is locked). Runtime and service unchanged. Unlock the login Keychain and grant the candidate access in Keychain Access, or re-sign it with the installed build's signing identity; then run `{command}` to check access without prompting. Cause: {source}", home.display())]
    StoreKey {
        home: PathBuf,
        command: String,
        #[source]
        source: Box<StoreKeyError>,
    },
    #[error("candidate preflight refused; runtime and service unchanged. After resolving trust/access, run `{command}` using the original candidate (the temporary probe copy is removed). Diagnostic: {detail}")]
    CandidateRefused { command: String, detail: String },
    #[error("candidate did not prove noninteractive Keychain store-key access for {}; runtime and service unchanged. Use a candidate that supports the store-key preflight, then retry your original update command", home.display())]
    MissingStoreKeyProof { home: PathBuf },
    #[error("candidate platform, schema check or handoff protocol mismatch; runtime and service unchanged")]
    Incompatible,
}

/// Quote paths as single POSIX shell arguments, including embedded apostrophes.
fn check_command(candidate: &Path, home: &Path) -> String {
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"));
    format!(
        "{} runtime service update-check --home {}",
        quote(candidate),
        quote(home)
    )
}

fn observation(error: anyhow::Error) -> PreflightError {
    PreflightError::Observation(gents_protocol::error_chain::PassedThrough::new(error))
}

pub(super) fn keychain_custody(encryption: Option<&StoreEncryption>) -> bool {
    encryption.is_some_and(|record| matches!(record.custody, StoreKeyCustody::MacosKeychain { .. }))
}

pub(super) fn check_store_key(
    home: &Path,
    encryption: Option<&StoreEncryption>,
) -> std::result::Result<bool, PreflightError> {
    check_store_key_with(home, encryption, StoreEncryption::load)
}

fn check_store_key_with(
    home: &Path,
    encryption: Option<&StoreEncryption>,
    read: impl FnOnce(&StoreEncryption, &Path, &Path) -> std::result::Result<StoreKey, StoreKeyError>,
) -> std::result::Result<bool, PreflightError> {
    let Some(record) = encryption.filter(|record| keychain_custody(Some(record))) else {
        return Ok(false);
    };
    let _no_prompts =
        crate::process_resources::prevent_keychain_prompts_guard().map_err(observation)?;
    let executable = std::env::current_exe().map_err(|error| observation(error.into()))?;
    read(
        record,
        &gents_store::local::store_key::home_key_file(home),
        &gents_store::local::home::default_data_dir(home),
    )
    .map(drop)
    .map_err(|source| PreflightError::StoreKey {
        home: home.to_path_buf(),
        command: check_command(&executable, home),
        source: Box::new(source),
    })?;
    Ok(true)
}

pub(super) fn probe(
    candidate: &Path,
    home: &Path,
    keychain: bool,
    original_candidate: &Path,
) -> std::result::Result<(), PreflightError> {
    super::files::validate_native_executable(candidate).map_err(observation)?;
    let output = ProcessCommandRunner
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
    if !output.success {
        return Err(PreflightError::CandidateRefused {
            command: check_command(original_candidate, home),
            detail: gents_protocol::failure_text::bounded_text(&output.stderr),
        });
    }
    let metadata = serde_json::from_str(&output.stdout)?;
    admit(&metadata, home, keychain)?;
    Ok(())
}

fn admit(
    metadata: &serde_json::Value,
    home: &Path,
    keychain: bool,
) -> std::result::Result<Phase, PreflightError> {
    let verified = metadata["handoff_version"] == 1
        && metadata["schema_checked"] == true
        && metadata["os"] == std::env::consts::OS
        && metadata["arch"] == std::env::consts::ARCH;
    let readable = metadata["keychain_store_key_checked"] == true;
    gents_protocol::serve_lifecycle::update::preflight(verified, keychain, readable).ok_or_else(
        || {
            if verified && keychain && !readable {
                PreflightError::MissingStoreKeyProof {
                    home: home.to_path_buf(),
                }
            } else {
                PreflightError::Incompatible
            }
        },
    )
}

#[cfg(test)]
mod tests;
