use rapidhash::RapidHashMap;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use gents_protocol::job::{JobOrigin, JobPin, JobPolicy, PROJECT_MOUNT_CLASS};

use crate::pack::home_install::recorded_provenance;
use crate::pack::{list_home_installs, read_home_install, InstalledVia, PackKind};
use crate::plugin::store::InstalledPlugin;
use crate::plugin_job::admit::Refusal;

#[derive(Debug, thiserror::Error)]
pub enum PluginJobError {
    #[error(
        "job is not a valid block; its fields are agent_targets, max_live, max_spawns, \
         max_messages_per_slot, result_bytes, max_wall_secs and policy"
    )]
    InvalidBlock(#[source] serde_json::Error),
    #[error(
        "plugin {plugin:?} ships a job block this install cannot grant. A pack-shipped job \
         block needs an operator install, job.agent_targets that name AgentTargets shipped \
         by {coordinate} or its dependencies, job.policy.trusted_packs within {coordinate} \
         and its dependencies, job.policy.dynamic off or check, and no \
         job.policy.project_roots. Next: drop the job block from the pack, or install the \
         pack with `gents pack install` and wire the block into Tools as the operator"
    )]
    JobBlockNotAdmissible { plugin: String, coordinate: String },
    #[error("reading {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("job row lacks physical identity")]
    JobRowLacksPhysicalIdentity,
    #[error("abort the staged kill")]
    AbortStagedKill,
    #[error("caused request query omitted rows")]
    CausedRequestQueryOmittedRows,
    #[error("decoding caused requests")]
    DecodingCausedRequests(#[source] serde_json::Error),
    #[error("decoding the resume target row")]
    DecodingResumeTargetRow(#[source] serde_json::Error),
    #[error("session job query omitted rows")]
    SessionJobQueryOmittedRows,
    #[error("decoding session job rows")]
    DecodingSessionJobRows(#[source] serde_json::Error),
    #[error("a job tool call has no physical identity")]
    JobToolCallHasNoPhysical,
    #[error("assembling the step 0 input")]
    AssemblingStepInput(#[source] crate::plugin_job::input::InputError),
    #[error("an admitted control entry has no target")]
    AdmittedControlEntryHasNoTarget,
    #[error("an admitted control entry has no job row")]
    AdmittedControlEntryHasNoJob,
    #[error("job row {row_doc_id} does not exist")]
    JobRowDoesNotExist { row_doc_id: String },
    #[error("an effects output without a pin was admitted")]
    EffectsOutputWithoutPinWasAdmitted,
    #[error("a step 0 answer has no terminal")]
    StepAnswerHasNoTerminal,
    #[error("AgentToolCall query omitted rows for the control targets")]
    AgentToolCallQueryOmittedRowsControlTargets,
    #[error("decoding the control targets")]
    DecodingControlTargets(#[source] serde_json::Error),
    #[error("an adopted output carries a pin")]
    AdoptedOutputCarriesPin,
    #[error("an adopted row has a pin")]
    AdoptedRowHasPin,
    #[error("the job deadline is out of range")]
    JobDeadlineOutRange,
    #[error("the call is no longer a running foreground call")]
    CallNoLongerRunningForegroundCall,
    #[error("a plugin without data cannot write")]
    PluginWithoutDataCannotWrite,
    #[error("only an effects output stays a job")]
    OnlyEffectsOutputStaysJob,
    #[error("the job_pin of {row_doc_id} changed during the step")]
    JobPinChangedDuringStep { row_doc_id: String },
    #[error("multiple Node rows matched a DID")]
    MultipleNodeRowsMatchedDID,
    #[error("a job step needs a local node")]
    JobStepNeedsLocalNode,
    #[error("job row {row_doc_id} has no job_pin")]
    JobRowHasNoJobPin { row_doc_id: String },
    #[error("the step of job row {row_doc_id} did not run")]
    StepJobRowDidNotRun {
        row_doc_id: String,
        #[source]
        source: StepRunFailure,
    },
    #[error("the step of job row {row_doc_id} returned {verdict}")]
    StepJobRowReturned {
        row_doc_id: String,
        verdict: String,
        #[source]
        source: crate::plugin_job::driver::StepFailure,
    },
    #[error("decoding the step output of job row {row_doc_id}")]
    DecodingStepOutputJobRow {
        row_doc_id: String,
        #[source]
        source: crate::plugin_job::driver::StepFailure,
    },
    #[error("plugin {plugin_ref} declares no data, so its step cannot write")]
    PluginDeclaresNoDataSoStep { plugin_ref: String },
    #[error("job row {row_doc_id} has no progress to carry")]
    JobRowHasNoProgressCarry { row_doc_id: String },
    #[error("Node hop-cap query returned no row array")]
    NodeHopCapQueryReturnedNo,
    #[error("Node.max_request_hop must be a nonnegative u32")]
    NodeMaxRequestHopMustNonnegative,
    #[error("job row {row_doc_id} does not exist")]
    JobRowDoesNotExist2 { row_doc_id: String },
    #[error("decoding job row {row_doc_id}")]
    DecodingJobRow {
        row_doc_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("the caller request of job row {row_doc_id} does not exist")]
    CallerRequestJobRowDoesNot { row_doc_id: String },
    #[error("decoding the accepted arguments of job row {row_doc_id}")]
    DecodingAcceptedArgumentsJobRow {
        row_doc_id: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "job staging integrity error: retry key {retry_key} does not match the staged request"
    )]
    JobStagingIntegrityErrorRetryKey { retry_key: String },
    #[error("job step recovery failed after ambiguous commit: {detail}; original commit error: {detail2}")]
    JobStepRecoveryFailedAfterAmbiguous { detail: String, detail2: String },
    #[error(
        "job staging integrity error: retry key {retry_key} does not match the admitted request"
    )]
    JobStagingIntegrityErrorRetryKey2 { retry_key: String },
    #[error("job staging integrity error: retry key {retry_key} is not unique")]
    JobStagingIntegrityErrorRetryKey3 { retry_key: String },
    #[error("commit acknowledgement lost")]
    CommitAcknowledgementLost,
    #[error("cwd is not utf-8")]
    CwdNotUtf,
    #[error("scratch file refused")]
    ScratchFileRefused,
    #[error("no admitted attempt for message slot")]
    NoAdmittedAttemptMessageSlot,
    #[error("staged request lacks a retry_key")]
    StagedRequestLacksRetryKey,
    #[error("spawn target is not in target_hops")]
    SpawnTargetNotTargetHops,
    #[error("decoding job request fingerprint")]
    DecodingJobRequestFingerprint(#[source] serde_json::Error),
    #[error("the caller request of job row {row_doc_id} does not exist")]
    CallerRequestJobRowDoesNot2 { row_doc_id: String },
    #[error("{reason}")]
    InterruptRefused { reason: String },
    #[error(transparent)]
    HomeInstall(#[from] crate::pack::home_install::HomeInstallError),
    #[error(transparent)]
    Mounts(#[from] crate::plugin::mounts::MountsError),
    #[error(transparent)]
    AdmissionCause(#[from] crate::request_admission::AdmissionCause),
    #[error(transparent)]
    ConfigClient(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Delivery(#[from] crate::tool_call_lifecycle::delivery::DeliveryError),
    #[error(transparent)]
    Identity(#[from] crate::identity::IdentityError),
    #[error(transparent)]
    Input(Box<crate::plugin_job::input::InputError>),
    #[error(transparent)]
    Interrupt(Box<crate::interrupt::InterruptError>),
    #[error(transparent)]
    JobRecords(#[from] crate::plugin_job::records::JobRecordsError),
    #[error(transparent)]
    PluginStore(#[from] crate::plugin::store::PluginStoreError),
    #[error(transparent)]
    PluginWrites(#[from] crate::application_write::PluginWritesError),
    #[error(transparent)]
    RequestBinding(#[from] crate::request_binding::RequestBindingError),
    #[error(transparent)]
    RequestReceipt(#[from] crate::request_admission::RequestReceiptError),
    #[error(transparent)]
    SessionMessage(#[from] crate::session_message::SessionMessageError),
    #[error(transparent)]
    ToolCallQuery(#[from] crate::tool_call_lifecycle::query::ToolCallQueryError),
    #[error(transparent)]
    TryFromInt(#[from] std::num::TryFromIntError),
    #[error(transparent)]
    BackgroundCompletion(#[from] crate::background_completion::BackgroundCompletionError),
    #[error(transparent)]
    Goal(Box<crate::goal::GoalError>),
    #[error(transparent)]
    Hook(Box<crate::hook::HookError>),
    #[error(transparent)]
    Did(#[from] identity::Error),
    #[error(transparent)]
    Materialize(Box<crate::lifecycle::materialize::MaterializeError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Session(Box<crate::session::SessionError>),
    #[error(transparent)]
    Std(#[from] std::io::Error),
    #[error(transparent)]
    Transition(#[from] crate::tool_call_lifecycle::TransitionError),
}

impl From<crate::lifecycle::materialize::MaterializeError> for PluginJobError {
    fn from(error: crate::lifecycle::materialize::MaterializeError) -> Self {
        Self::Materialize(Box::new(error))
    }
}

impl From<crate::session::SessionError> for PluginJobError {
    fn from(error: crate::session::SessionError) -> Self {
        Self::Session(Box::new(error))
    }
}

impl From<crate::hook::HookError> for PluginJobError {
    fn from(error: crate::hook::HookError) -> Self {
        Self::Hook(Box::new(error))
    }
}

impl From<crate::goal::GoalError> for PluginJobError {
    fn from(error: crate::goal::GoalError) -> Self {
        Self::Goal(Box::new(error))
    }
}

impl From<crate::interrupt::InterruptError> for PluginJobError {
    fn from(error: crate::interrupt::InterruptError) -> Self {
        Self::Interrupt(Box::new(error))
    }
}

impl crate::config_client::TransactionFailure for PluginJobError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::AssemblingStepInput(error) => error.config_client_error(),
            Self::AdmissionCause(error) => error.config_client_error(),
            Self::ConfigClient(error) => Some(error),
            Self::Delivery(error) => error.config_client_error(),
            Self::Input(error) => error.config_client_error(),
            Self::Interrupt(error) => error.config_client_error(),
            Self::JobRecords(error) => error.config_client_error(),
            Self::PluginWrites(error) => error.config_client_error(),
            Self::SessionMessage(error) => error.config_client_error(),
            Self::ToolCallQuery(error) => error.config_client_error(),
            Self::Goal(error) => error.config_client_error(),
            Self::Hook(error) => error.config_client_error(),
            Self::Materialize(error) => error.config_client_error(),
            Self::Session(error) => error.config_client_error(),
            Self::Transition(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl From<crate::plugin_job::input::InputError> for PluginJobError {
    fn from(error: crate::plugin_job::input::InputError) -> Self {
        Self::Input(Box::new(error))
    }
}

/// Why a job step did not run: a classified guest failure or the run error.
#[derive(Debug, thiserror::Error)]
pub enum StepRunFailure {
    #[error(transparent)]
    Failure(crate::plugin_job::driver::StepFailure),
    #[error(transparent)]
    Run(crate::plugin::executor::PluginRunError),
}

impl PluginJobError {
    /// The classified guest failure a step error carries, if any.
    pub(crate) fn step_failure(&self) -> Option<&crate::plugin_job::driver::StepFailure> {
        match self {
            Self::StepJobRowDidNotRun {
                source: StepRunFailure::Failure(failure),
                ..
            }
            | Self::StepJobRowReturned { source: failure, .. }
            | Self::DecodingStepOutputJobRow { source: failure, .. } => Some(failure),
            _ => None,
        }
    }
}

/// The files every resolved mount of one call holds, with the provenance
/// facts [`classify_origin`] reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptIndex {
    /// `(sha256, pack coordinate, path, mount class)` of every resolved
    /// mount's files, project mounts included.
    pub files: Vec<(String, String, String, String)>,
    /// Pack coordinate to its recorded provenance.
    pub installed_via: BTreeMap<String, InstalledVia>,
    pub trusted_packs: Vec<String>,
}

/// The host's origin of a pinned script; installed carries the bytes read
/// and verified against the pin's `source_digest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Installed {
        verified_script: Vec<u8>,
        digest: String,
    },
    Dynamic,
}

/// Lean `trustedHint`: a mount's `trusted` is true only for a pack-tier class
/// of an operator-installed pack that `trusted_packs` lists. The host still
/// decides origin at adoption.
pub fn trusted_hint(
    pack: &str,
    class: &str,
    installed_via: &BTreeMap<&str, InstalledVia>,
    trusted_packs: &[String],
) -> bool {
    class != PROJECT_MOUNT_CLASS
        && installed_via.get(pack) == Some(&InstalledVia::Operator)
        && trusted_packs.iter().any(|trusted| trusted == pack)
}

/// Lean `originOf`: installed exactly when some file with `digest` lies in a
/// mount [`trusted_hint`] accepts.
pub fn classify_origin(
    digest: &str,
    index: &[(String, String, String, String)],
    installed_via: &BTreeMap<&str, InstalledVia>,
    trusted_packs: &[String],
) -> JobOrigin {
    if index.iter().any(|(sha, pack, _, class)| {
        sha == digest && trusted_hint(pack, class, installed_via, trusted_packs)
    }) {
        JobOrigin::Installed
    } else {
        JobOrigin::Dynamic
    }
}

/// The origin of `pin`'s script. An installed script whose file no longer
/// hashes to the pinned digest is dynamic.
pub fn classify(index: &ScriptIndex, pin: &JobPin) -> Origin {
    let digest = pin.source_digest.as_str();
    let installed_via: BTreeMap<&str, InstalledVia> = index
        .installed_via
        .iter()
        .map(|(pack, via)| (pack.as_str(), *via))
        .collect();
    let trusted = &index.trusted_packs;
    if classify_origin(digest, &index.files, &installed_via, trusted) == JobOrigin::Dynamic {
        return Origin::Dynamic;
    }
    index
        .files
        .iter()
        .filter(|(sha, pack, _, class)| {
            sha == digest && trusted_hint(pack, class, &installed_via, trusted)
        })
        .find_map(|(_, _, path, _)| {
            let bytes = std::fs::read(path).ok()?;
            (sha256(&bytes) == digest).then(|| Origin::Installed {
                verified_script: bytes,
                digest: digest.to_owned(),
            })
        })
        .unwrap_or(Origin::Dynamic)
}

/// The [`ScriptIndex`] of one call of `plugin`, over the mounts
/// [`crate::plugin::mounts::resolve`] gives it.
pub fn script_index(
    home: &Path,
    plugin: &InstalledPlugin,
    policy: Option<&JobPolicy>,
    cwd: Option<&Path>,
) -> Result<ScriptIndex, PluginJobError> {
    let mut files = Vec::new();
    for mount in crate::plugin::mounts::resolve(home, plugin, policy, cwd)? {
        for (sha, path) in mount_files(&mount.pack, &mount.digest, &mount.path)? {
            files.push((sha, mount.pack.clone(), path, mount.class.clone()));
        }
    }
    let installed_via = list_home_installs(home)?
        .into_iter()
        .map(|record| {
            let via = recorded_provenance(record.kind, Some(record.installed_via));
            (record.coordinate, via)
        })
        .collect();
    Ok(ScriptIndex {
        files,
        installed_via,
        trusted_packs: policy.map_or_else(Vec::new, |policy| policy.trusted_packs.clone()),
    })
}

/// `(sha256, path)` of every regular file under a mount. A pack mount is a
/// digest-addressed dir that never changes in place, so its hashes are cached
/// by `(pack coordinate, digest, path)`; a project mount has no digest and is
/// hashed on every call.
fn mount_files(
    pack: &str,
    digest: &str,
    path: &str,
) -> Result<Vec<(String, String)>, PluginJobError> {
    type Cache = Mutex<RapidHashMap<(String, String, String), Vec<(String, String)>>>;
    static CACHE: LazyLock<Cache> = LazyLock::new(Default::default);
    let key = (pack.to_owned(), digest.to_owned(), path.to_owned());
    if !digest.is_empty() {
        if let Some(files) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Ok(files.clone());
        }
    }
    let mut files = Vec::new();
    hash_dir(Path::new(path), &mut files)?;
    if !digest.is_empty() {
        CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, files.clone());
    }
    Ok(files)
}

/// Symlinks are skipped, so a mount never indexes a file outside itself.
fn hash_dir(dir: &Path, files: &mut Vec<(String, String)>) -> Result<(), PluginJobError> {
    let read = |source| PluginJobError::Read {
        path: dir.to_path_buf(),
        source,
    };
    for entry in std::fs::read_dir(dir).map_err(read)? {
        let entry = entry.map_err(read)?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|source| PluginJobError::Read {
            path: path.clone(),
            source,
        })?;
        if kind.is_dir() {
            hash_dir(&path, files)?;
        } else if kind.is_file() {
            let bytes = std::fs::read(&path).map_err(|source| PluginJobError::Read {
                path: path.clone(),
                source,
            })?;
            files.push((sha256(&bytes), path.to_string_lossy().into_owned()));
        }
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("sha256:{:x}", sha2::Sha256::digest(bytes))
}

/// The engine guard: `plugin`'s owning pack must read `Operator` under
/// [`recorded_provenance`]. An unreadable record fails closed.
pub fn engine_guard(home: &Path, plugin: &InstalledPlugin) -> Result<(), Refusal> {
    let record = plugin
        .owner_pack_coordinate
        .as_deref()
        .and_then(|pack| read_home_install(home, pack).ok().flatten());
    let kind = record
        .as_ref()
        .map_or(PackKind::Documents, |record| record.kind.clone());
    let via = recorded_provenance(kind, record.map(|record| record.installed_via));
    if via == InstalledVia::Operator {
        Ok(())
    } else {
        Err(Refusal::NotAvailable)
    }
}

/// The call that follows an [`engine_guard`] refusal.
pub fn engine_guard_next(plugin: &InstalledPlugin) -> String {
    let pack = plugin
        .owner_pack_coordinate
        .clone()
        .unwrap_or_else(|| format!("{}/{}", plugin.namespace, plugin.name));
    format!("Next: the operator reinstalls {pack} with `gents pack install`")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::InstalledVia;
    use std::collections::BTreeMap;

    #[test]
    fn trusted_hint_is_operator_trusted_pack_tier_only() {
        let via: BTreeMap<&str, InstalledVia> = BTreeMap::from([
            ("acme/op", InstalledVia::Operator),
            ("acme/self", InstalledVia::SelfConfig),
        ]);
        let trusted = vec![
            "acme/op".to_owned(),
            "acme/self".to_owned(),
            "acme/ghost".to_owned(),
        ];
        assert!(trusted_hint("acme/op", "workflows", &via, &trusted));
        assert!(!trusted_hint("acme/op", "project", &via, &trusted));
        assert!(!trusted_hint("acme/self", "workflows", &via, &trusted));
        assert!(!trusted_hint("acme/ghost", "workflows", &via, &trusted));
        assert!(!trusted_hint("acme/op", "workflows", &via, &[]));
        let other = vec!["acme/other".to_owned()];
        assert!(!trusted_hint("acme/op", "workflows", &via, &other));
    }

    use crate::pack::{write_home_install, HomePackInstall, PackKind};
    use crate::plugin::store::InstalledPlugin;
    use crate::plugin_job::admit::Refusal;
    use crate::tool_call_lifecycle::admission_fixture::fixture_job_pin;
    use gents_protocol::job::{AssetMountDeclaration, JobOrigin, JobPin, JobPolicy};
    use std::path::Path;

    const SCRIPT: &[u8] = b"fn step(input) { input }";

    fn sha(bytes: &[u8]) -> String {
        use sha2::Digest;
        format!("sha256:{:x}", sha2::Sha256::digest(bytes))
    }

    fn plugin() -> InstalledPlugin {
        InstalledPlugin {
            namespace: "acme".into(),
            name: "engine".into(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "e".repeat(64)),
            language: "rust".into(),
            declaration: crate::pack::PackPlugin {
                name: "engine".into(),
                asset_mounts: vec![AssetMountDeclaration {
                    class: "workflows".into(),
                }],
                ..Default::default()
            },
            granted: None,
            instructions: None,
            owner_pack_coordinate: Some("acme/engine".into()),
            owner_pack_digest: None,
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
        }
    }

    /// Records `coordinate` as `kind` installed `via`, with `script` under
    /// `workflows/` of a materialized dir at `hex`.
    fn install(
        home: &Path,
        coordinate: &str,
        hex: &str,
        kind: PackKind,
        via: Option<InstalledVia>,
        script: &[u8],
    ) {
        let assets = format!("packs/.materialized/{coordinate}/{hex}");
        let dir = home.join(&assets).join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.rhai"), script).unwrap();
        write_home_install(
            home,
            &HomePackInstall {
                coordinate: coordinate.into(),
                version: "1.0.0".into(),
                digest: format!("sha256:{hex}"),
                kind,
                assets,
                dependency_assets: vec![],
                plugins: vec![],
                ux: Vec::new(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_via: via,
            },
        )
        .unwrap();
    }

    fn policy(trusted: &[&str]) -> JobPolicy {
        JobPolicy {
            trusted_packs: trusted.iter().map(|pack| (*pack).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn pin(digest: &str) -> JobPin {
        JobPin {
            source_digest: digest.to_owned(),
            ..fixture_job_pin()
        }
    }

    fn classify_in(home: &Path, trusted: &[&str], script: &[u8]) -> Origin {
        let index = script_index(home, &plugin(), Some(&policy(trusted)), None).unwrap();
        classify(&index, &pin(&sha(script)))
    }

    #[test]
    fn self_config_installed_pack_script_is_dynamic() {
        let home = tempfile::tempdir().unwrap();
        let via = Some(InstalledVia::SelfConfig);
        install(
            home.path(),
            "acme/sc",
            "11",
            PackKind::Documents,
            via,
            SCRIPT,
        );
        assert_eq!(
            classify_in(home.path(), &["acme/sc"], SCRIPT),
            Origin::Dynamic
        );
    }

    #[test]
    fn operator_installed_trusted_pack_script_is_installed_with_bytes() {
        let home = tempfile::tempdir().unwrap();
        let via = Some(InstalledVia::Operator);
        install(
            home.path(),
            "acme/op",
            "22",
            PackKind::Documents,
            via,
            SCRIPT,
        );
        assert_eq!(
            classify_in(home.path(), &["acme/op"], SCRIPT),
            Origin::Installed {
                verified_script: SCRIPT.to_vec(),
                digest: sha(SCRIPT),
            }
        );
        let other = b"other script";
        assert_eq!(
            classify_in(home.path(), &["acme/op"], other),
            Origin::Dynamic
        );
    }

    #[test]
    fn untrusted_pack_script_is_dynamic() {
        let home = tempfile::tempdir().unwrap();
        let via = Some(InstalledVia::Operator);
        install(
            home.path(),
            "acme/op",
            "33",
            PackKind::Documents,
            via,
            SCRIPT,
        );
        assert_eq!(classify_in(home.path(), &[], SCRIPT), Origin::Dynamic);
        assert_eq!(
            classify_in(home.path(), &["acme/x"], SCRIPT),
            Origin::Dynamic
        );
    }

    #[test]
    fn project_tier_script_is_dynamic() {
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let workflows = cwd.path().join(".gents/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(workflows.join("flow.rhai"), SCRIPT).unwrap();
        let policy = JobPolicy {
            project_roots: vec![cwd.path().canonicalize().unwrap().display().to_string()],
            ..policy(&[""])
        };
        let index = script_index(home.path(), &plugin(), Some(&policy), Some(cwd.path())).unwrap();
        assert!(index
            .files
            .iter()
            .any(|(digest, _, _, class)| *digest == sha(SCRIPT) && class == PROJECT_MOUNT_CLASS));
        assert_eq!(classify(&index, &pin(&sha(SCRIPT))), Origin::Dynamic);

        let index = ScriptIndex {
            files: vec![(
                sha(SCRIPT),
                "acme/op".into(),
                workflows.join("flow.rhai").display().to_string(),
                PROJECT_MOUNT_CLASS.into(),
            )],
            installed_via: BTreeMap::from([("acme/op".to_owned(), InstalledVia::Operator)]),
            trusted_packs: vec!["acme/op".into()],
        };
        assert_eq!(classify(&index, &pin(&sha(SCRIPT))), Origin::Dynamic);
    }

    #[test]
    fn engine_pack_installed_by_self_config_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let refused = |home: &Path| {
            let refusal = engine_guard(home, &plugin()).unwrap_err();
            assert_eq!(refusal, Refusal::NotAvailable);
            assert_eq!(refusal.code(), Some("plugin_job_not_available"));
        };
        refused(home.path());
        let via = Some(InstalledVia::SelfConfig);
        install(
            home.path(),
            "acme/engine",
            "44",
            PackKind::Plugins,
            via,
            SCRIPT,
        );
        refused(home.path());
        assert_eq!(
            engine_guard_next(&plugin()),
            "Next: the operator reinstalls acme/engine with `gents pack install`"
        );
    }

    #[test]
    fn engine_installed_through_the_documents_route_passes() {
        let home = tempfile::tempdir().unwrap();
        let via = Some(InstalledVia::Operator);
        install(
            home.path(),
            "acme/engine",
            "55",
            PackKind::Documents,
            via,
            SCRIPT,
        );
        assert_eq!(engine_guard(home.path(), &plugin()), Ok(()));
    }

    #[test]
    fn script_index_is_cached_by_plugin_coordinate_and_digest() {
        let home = tempfile::tempdir().unwrap();
        let via = Some(InstalledVia::Operator);
        install(
            home.path(),
            "acme/cached",
            "66",
            PackKind::Documents,
            via,
            SCRIPT,
        );
        assert!(matches!(
            classify_in(home.path(), &["acme/cached"], SCRIPT),
            Origin::Installed { .. }
        ));
        // A digest-addressed dir never changes in place: the cached index
        // keeps the first hash, and the changed bytes no longer verify.
        let rewritten = b"rewritten in place";
        let file = home
            .path()
            .join("packs/.materialized/acme/cached/66/workflows/flow.rhai");
        std::fs::write(&file, rewritten).unwrap();
        assert_eq!(
            classify_in(home.path(), &["acme/cached"], rewritten),
            Origin::Dynamic
        );
        assert_eq!(
            classify_in(home.path(), &["acme/cached"], SCRIPT),
            Origin::Dynamic
        );
        // A new digest of the pack is indexed afresh.
        install(
            home.path(),
            "acme/cached",
            "77",
            PackKind::Documents,
            via,
            rewritten,
        );
        assert!(matches!(
            classify_in(home.path(), &["acme/cached"], rewritten),
            Origin::Installed { .. }
        ));
    }

    #[test]
    fn classify_origin_is_lean_origin_of() {
        let entry = |pack: &str, class: &str| {
            (
                "sha256:d".to_owned(),
                pack.to_owned(),
                "/m/flow.rhai".to_owned(),
                class.to_owned(),
            )
        };
        let via: BTreeMap<&str, InstalledVia> = BTreeMap::from([
            ("acme/op", InstalledVia::Operator),
            ("acme/self", InstalledVia::SelfConfig),
            ("acme/untrusted", InstalledVia::Operator),
        ]);
        let trusted = vec![
            "acme/op".to_owned(),
            "acme/self".to_owned(),
            "acme/ghost".to_owned(),
        ];
        let origin = |index: &[(String, String, String, String)]| {
            classify_origin("sha256:d", index, &via, &trusted)
        };
        assert_eq!(
            origin(&[entry("acme/op", "workflows")]),
            JobOrigin::Installed
        );
        assert_eq!(
            origin(&[entry("acme/op", PROJECT_MOUNT_CLASS)]),
            JobOrigin::Dynamic
        );
        assert_eq!(
            origin(&[entry("acme/self", "workflows")]),
            JobOrigin::Dynamic
        );
        assert_eq!(
            origin(&[entry("acme/ghost", "workflows")]),
            JobOrigin::Dynamic
        );
        assert_eq!(
            origin(&[entry("acme/untrusted", "workflows")]),
            JobOrigin::Dynamic
        );
        assert_eq!(origin(&[]), JobOrigin::Dynamic);
        assert_eq!(
            classify_origin(
                "sha256:other",
                &[entry("acme/op", "workflows")],
                &via,
                &trusted
            ),
            JobOrigin::Dynamic
        );
    }
}
