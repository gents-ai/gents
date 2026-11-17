use super::*;
use crate::native_service::CommandRunner;
use gents_store::local::store_key::{StoreEncryption, StoreKey, StoreKeyCustody, StoreKeyError};

#[derive(Debug, thiserror::Error)]
pub(super) enum PreflightError {
    #[error("candidate preflight could not complete; runtime and service unchanged")]
    Observation(#[source] gents_protocol::error_chain::PassedThrough),
    #[error("candidate preflight returned invalid metadata; runtime and service unchanged")]
    Metadata(#[from] serde_json::Error),
    #[error("candidate {} cannot read this home's store key without interaction ({}). Keychain service `{service}`, account `{account}`. Runtime and service unchanged. A person at the Mac must approve this build's access to that item (and unlock the login Keychain if needed), or install a build signed with the same stable identity as the running one. Then run `{command}` to CHECK access without prompting, and retry your original update command. Cause: {source}", candidate.display(), home.display())]
    StoreKey {
        candidate: PathBuf,
        home: PathBuf,
        service: &'static str,
        account: String,
        command: String,
        #[source]
        source: Box<StoreKeyCause>,
    },
    #[error("candidate preflight failed; runtime and service unchanged. Diagnostic: {detail}")]
    CandidateRefused { detail: String },
    #[error("candidate did not prove noninteractive Keychain store-key access for {}; runtime and service unchanged. Use a candidate that supports the store-key preflight, then retry your original update command", home.display())]
    MissingStoreKeyProof { home: PathBuf },
    #[error("candidate platform, schema check or handoff protocol mismatch; runtime and service unchanged")]
    Incompatible,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum StoreKeyCause {
    #[error(transparent)]
    Local(#[from] StoreKeyError),
    #[error("{detail}")]
    Candidate { detail: String },
}

/// Versioned child evidence: never scrape stderr for a Keychain-looking phrase.
#[derive(Debug, Serialize, Deserialize)]
struct FailureEnvelope {
    store_key_failure_v1: String,
}

pub(super) fn failure_metadata(error: &anyhow::Error) -> Option<serde_json::Value> {
    match error.downcast_ref::<PreflightError>()? {
        PreflightError::StoreKey { source, .. } => Some(serde_json::json!(FailureEnvelope {
            store_key_failure_v1: gents_protocol::failure_text::bounded_text(&source.to_string()),
        })),
        _ => None,
    }
}

fn store_key_error(
    home: &Path,
    candidate: &Path,
    account: String,
    source: StoreKeyCause,
) -> PreflightError {
    PreflightError::StoreKey {
        candidate: candidate.to_owned(),
        home: home.to_owned(),
        service: "com.source-inc.gents.store-key",
        account,
        command: check_command(candidate, home),
        source: Box::new(source),
    }
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
    .map_err(|source| {
        let StoreKeyCustody::MacosKeychain { keychain_label } = &record.custody else {
            unreachable!()
        };
        store_key_error(home, &executable, keychain_label.clone(), source.into())
    })?;
    Ok(true)
}

pub(super) fn probe(
    runner: &impl CommandRunner,
    candidate: &Path,
    home: &Path,
    keychain: bool,
    original_candidate: &Path,
) -> std::result::Result<Phase, PreflightError> {
    super::files::validate_native_executable(candidate).map_err(observation)?;
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
    if !output.success {
        if keychain {
            if let Ok(failure) = serde_json::from_str::<FailureEnvelope>(&output.stdout) {
                let config = read_init_config(home).map_err(observation)?;
                if let Some(StoreEncryption {
                    custody: StoreKeyCustody::MacosKeychain { keychain_label },
                    ..
                }) = config.and_then(|config| config.store_encryption)
                {
                    return Err(store_key_error(
                        home,
                        original_candidate,
                        keychain_label,
                        StoreKeyCause::Candidate {
                            detail: failure.store_key_failure_v1,
                        },
                    ));
                }
            }
        }
        return Err(PreflightError::CandidateRefused {
            detail: gents_protocol::failure_text::bounded_text(&output.stderr),
        });
    }
    let metadata = serde_json::from_str(&output.stdout)?;
    admit(&metadata, home, keychain)
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
