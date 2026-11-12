//! Constrained, independently supervised native-service replacement. Durable
//! receipts survive runtime retirement; they never stand in for store locks or
//! the runtime's authenticated admission fence.
use crate::args::ServiceUpdateArgs;
use crate::home_state::{read_init_config, read_runtime_state, resolve_home_dir};
use crate::native_service::{NativeServiceConfig, NativeServiceManager};
use crate::network_settings::NetworkSettings;
use anyhow::{Context, Result};
use gents_protocol::serve_lifecycle::update::{Action, Phase};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod execute;
mod files;
mod notices;
mod recovery;
mod stage;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Intent {
    digest: String,
    executable_update: bool,
    network: NetworkSettings,
    environment: Option<std::collections::BTreeMap<String, String>>,
    drain_timeout_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Plan {
    version: u32,
    id: uuid::Uuid,
    home: PathBuf,
    node_did: String,
    session_id: Option<String>,
    request_id: Option<String>,
    endpoint: String,
    prior_build: String,
    intent: Intent,
    prior_network: NetworkSettings,
    prior_definition: Vec<u8>,
    next_definition: Vec<u8>,
    enabled: bool,
    accepted_deadline: String,
    p2p_enabled: bool,
    prior_peer_id: Option<String>,
    service_config_dir: PathBuf,
    user_home: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct HelperReady {
    operation_id: uuid::Uuid,
    pid: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    operation_id: uuid::Uuid,
    node_did: String,
    session_id: Option<String>,
    request_id: Option<String>,
    phase: Phase,
    message: String,
    updated_at: String,
    directory: PathBuf,
}
impl Receipt {
    async fn transition(&mut self, action: Action, message: &str) -> Result<()> {
        let mut receipt = self.clone();
        let message = message.to_owned();
        receipt = tokio::task::spawn_blocking(move || {
            receipt.transition_sync(action, &message)?;
            Ok::<_, anyhow::Error>(receipt)
        })
        .await??;
        *self = receipt;
        Ok(())
    }

    fn transition_sync(&mut self, action: Action, message: &str) -> Result<()> {
        let expected = self.phase;
        let mut receipt = self.clone();
        receipt.phase = expected
            .step(action)
            .context("invalid managed update transition")?;
        receipt.message = message.to_owned();
        receipt.updated_at = chrono::Utc::now().to_rfc3339();
        // The helper and submitting process share a receipt, so a stale
        // acknowledgement must never overwrite a terminal helper failure.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(receipt.directory.join("receipt.lock"))?;
        lock.lock()?;
        let path = receipt.directory.join("receipt.json");
        let current: Receipt = files::read_json(&path)?;
        anyhow::ensure!(
            current.operation_id == receipt.operation_id && current.phase == expected,
            "managed update receipt changed; read service update-status"
        );
        files::write_json(&path, &receipt)?;
        *self = receipt;
        Ok(())
    }
}

fn service_config(plan: &Plan) -> Result<NativeServiceConfig> {
    let mut config = NativeServiceConfig::new(
        plan.home.clone(),
        operation_dir(&plan.home, plan.id).join("gents"),
    )?;
    if !plan.intent.executable_update {
        config.executable = NativeServiceManager::new(config.clone())?
            .definition_executable(&plan.prior_definition)?;
    }
    config.service_config_dir = plan.service_config_dir.clone();
    config.user_home = plan.user_home.clone();
    Ok(config)
}

fn operation_dir(home: &Path, id: uuid::Uuid) -> PathBuf {
    home.join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME)
        .join(id.to_string())
}

async fn emit(value: &impl Serialize) -> Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&bytes).await?;
    stdout.flush().await?;
    Ok(())
}

pub async fn submit(args: ServiceUpdateArgs) -> Result<()> {
    submit_to(args, None, &mut tokio::io::stdout())
        .await
        .map(drop)
}

async fn submit_to<W>(
    args: ServiceUpdateArgs,
    helper: Option<PathBuf>,
    writer: &mut W,
) -> Result<Receipt>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let home = tokio::fs::canonicalize(resolve_home_dir(args.home.as_deref())).await?;
    let id = args.operation_id.unwrap_or_else(uuid::Uuid::new_v4);
    let (plan, mut receipt, _submission_lock) =
        tokio::task::spawn_blocking(move || stage::prepare(home, id, args, helper)).await??;
    if receipt.phase == Phase::Staged {
        let registration = async {
            execute::validate_live(&plan).await?;
            let copy = plan.clone();
            tokio::task::spawn_blocking(move || {
                NativeServiceManager::new(service_config(&copy)?)?
                    .register_updater(&operation_dir(&copy.home, copy.id), copy.id)
            })
            .await??;
            wait_for_helper(&plan, Duration::from_secs(20)).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = registration {
            receipt
                .transition(
                    Action::Fail,
                    &format!(
                        "Preflight or helper registration failed; runtime unchanged: {error:#}"
                    ),
                )
                .await?;
            return Err(error.context(format!(
                "update {} failed; inspect `gents runtime service update-status --home {} {}`",
                plan.id,
                plan.home.display(),
                plan.id
            )));
        }
        receipt
            .transition(
                Action::Register(true),
                "Independent updater registered; no retirement before acknowledgement",
            )
            .await?;
    }
    if !matches!(receipt.phase, Phase::Healthy | Phase::Failed) {
        // An acknowledged operation may outlive a host reboot or an unloaded
        // helper job. Re-register the same constrained job, never a second plan.
        let copy = plan.clone();
        tokio::task::spawn_blocking(move || {
            let directory = operation_dir(&copy.home, copy.id);
            register_unless_helper_runs(&directory, || {
                NativeServiceManager::new(service_config(&copy)?)?
                    .register_updater(&directory, copy.id)
            })
        })
        .await??;
        if receipt.phase == Phase::Registered {
            wait_for_helper(&plan, Duration::from_secs(20)).await?;
        }
    }
    if receipt.phase == Phase::Registered {
        acknowledge(&mut receipt, writer).await?;
        return Ok(receipt);
    }
    use tokio::io::AsyncWriteExt as _;
    let mut bytes = serde_json::to_vec(&receipt)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(receipt)
}

async fn wait_for_helper(plan: &Plan, limit: Duration) -> Result<()> {
    let path = operation_dir(&plan.home, plan.id).join("helper-ready.json");
    tokio::time::timeout(limit, async {
        let mut changes =
            files::Changes::watch(path.parent().context("operation directory")?).await?;
        loop {
            if tokio::fs::try_exists(&path).await? {
                let ready: HelperReady = files::read_json_async(&path).await?;
                anyhow::ensure!(
                    ready.operation_id == plan.id && ready.pid != 0,
                    "helper readiness belongs to a different operation"
                );
                return Ok::<_, anyhow::Error>(());
            }
            changes.changed().await?;
        }
    })
    .await
    .context("native updater did not acknowledge startup; runtime unchanged")?
}

async fn acknowledge<W>(receipt: &mut Receipt, writer: &mut W) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    let mut accepted = receipt.clone();
    accepted.phase = Phase::Accepted;
    accepted.message =
        "Accepted, not completed. Finish this turn; read service update-status after reconnect."
            .into();
    let mut bytes = serde_json::to_vec(&accepted)?;
    bytes.push(b'\n');
    // Do not release the helper on a failed or unflushed acknowledgement.
    // The runtime's existing busy fence still waits for this tool and turn.
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    receipt
        .transition(Action::Acknowledge, &accepted.message)
        .await
}

/// A running helper holds its operation's lock from start through its terminal
/// cleanup, and owns its registration until then: that cleanup removes the job
/// definition, so re-registering beside it races the removal (`systemctl start`
/// of a unit just deleted). Re-register only an operation whose helper is not
/// running, holding the lock while doing so. Returns whether it registered.
fn register_unless_helper_runs(
    directory: &Path,
    register: impl FnOnce() -> Result<()>,
) -> Result<bool> {
    let Ok(_helper) = files::lock(&directory.join("helper.lock")) else {
        return Ok(false);
    };
    register()?;
    Ok(true)
}

pub async fn status(home: Option<PathBuf>, id: uuid::Uuid) -> Result<()> {
    let home = resolve_home_dir(home.as_deref());
    let receipt = tokio::task::spawn_blocking(move || -> Result<Receipt> {
        let directory = operation_dir(&home, id);
        let receipt: Receipt = files::read_json(&directory.join("receipt.json"))?;
        if matches!(
            receipt.phase,
            Phase::Accepted | Phase::Retired | Phase::Released | Phase::BackedUp | Phase::Installed
        ) {
            let plan: Plan = files::read_json(&directory.join("plan.json"))?;
            anyhow::ensure!(
                plan.id == id && plan.home == std::fs::canonicalize(&home)?,
                "update plan does not match home"
            );
            register_unless_helper_runs(&directory, || {
                NativeServiceManager::new(service_config(&plan)?)?.register_updater(&directory, id)
            })?;
        }
        Ok(receipt)
    })
    .await??;
    emit(&receipt).await
}

pub use notices::watch_notices;
pub fn executable_digest(path: &Path) -> Result<String> {
    files::digest(path)
}

pub async fn check(home: Option<PathBuf>) -> Result<()> {
    emit(&check_metadata(home).await?).await
}

async fn check_metadata(home: Option<PathBuf>) -> Result<serde_json::Value> {
    let Some(home) = home else {
        return Ok(metadata(false));
    };
    let _keychain = crate::process_resources::prevent_keychain_prompts(true)?;
    let (identity, state, settings) = tokio::task::spawn_blocking(move || -> Result<_> {
        gents_store::local::storage_backend::reject_legacy_store(
            &gents_store::local::home::default_data_dir(&home),
        )?;
        let config = read_init_config(&home)?.context("home must be initialized")?;
        let state = read_runtime_state(&home)?.context("runtime endpoint missing")?;
        let settings = NetworkSettings::read(&home)?;
        let identity = crate::home_state::load_initialized_home_identity(&home, &config)?;
        anyhow::ensure!(
            state.node_did == config.node_did,
            "runtime identity mismatch"
        );
        Ok((identity, state, settings))
    })
    .await??;
    execute::endpoint(&state.graphql, &settings)?;
    let access = gents_store::config_client::ConfigAccess::Graphql(
        gents_store::config_client::GraphqlEndpoint::as_principal(state.graphql, identity.did())
            .with_client_signing_capability(identity.client_signing_capability()?)?,
    );
    let versions = access
        .collection_versions()
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<gents::defra_node::CollectionVersion>, _>>()?;
    gents_store::schema::migration::check_migration_compatibility(&versions)?;
    let lineage = gents_store::schema::migration::non_canonical_store_lineages(&versions)?;
    for mismatch in &lineage {
        eprintln!("warning: {mismatch}");
    }
    let mut metadata = metadata(true);
    metadata["non_canonical_lineages"] = serde_json::json!(lineage);
    Ok(metadata)
}

fn metadata(schema_checked: bool) -> serde_json::Value {
    serde_json::json!({"handoff_version":1,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "schema_checked":schema_checked,"service_label":crate::native_service::SERVICE_LABEL,
        "systemd_unit":crate::native_service::SYSTEMD_UNIT})
}

pub async fn run(home: PathBuf, id: uuid::Uuid) -> Result<()> {
    let guarded_home = home.clone();
    tokio::task::spawn_blocking(move || {
        crate::lifecycle::refuse_update_from_its_runtime(&guarded_home)
    })
    .await??;
    let directory = operation_dir(&home, id);
    let lock_dir = directory.clone();
    let _lock =
        tokio::task::spawn_blocking(move || files::lock(&lock_dir.join("helper.lock"))).await??;
    let plan: Plan = files::read_json_async(&directory.join("plan.json")).await?;
    anyhow::ensure!(
        plan.version == 1 && plan.id == id && plan.home == tokio::fs::canonicalize(&home).await?,
        "update plan does not match helper invocation"
    );
    let mut receipt: Receipt = files::read_json_async(&directory.join("receipt.json")).await?;
    if matches!(receipt.phase, Phase::Healthy | Phase::Failed) {
        return cleanup_helper(&plan).await;
    }
    // This is a readiness observation, not authority to retire. The helper
    // still authenticates the node/build through the existing control owner.
    let ready = HelperReady {
        operation_id: plan.id,
        pid: std::process::id(),
    };
    let ready_path = directory.join("helper-ready.json");
    tokio::task::spawn_blocking(move || files::write_json(&ready_path, &ready)).await??;
    let mut reservation = None;
    let result = execute::apply(&plan, &mut receipt, &mut reservation).await;
    if let Err(error) = result {
        // No automatic binary rollback: opening a newer store can make the
        // old executable incompatible. Keep the cold backup for explicit recovery.
        let recovery = recovery::restore_unstarted(&plan, receipt.phase, &mut reservation).await;
        let detail = match recovery {
            Ok(true) => "Prior runtime restored using the verified compatible build; store data was not rolled back.".to_owned(),
            Ok(false) => "No data reset or automatic store rollback. Inspect this receipt and backup before external recovery.".to_owned(),
            Err(error) => format!("Prior runtime recovery unavailable: {error:#}. No data reset or automatic store rollback."),
        };
        let current: Receipt = files::read_json_async(&directory.join("receipt.json")).await?;
        if !matches!(current.phase, Phase::Healthy | Phase::Failed) {
            receipt = current;
            receipt
                .transition(Action::Fail, &format!("{error:#}. {detail}"))
                .await?;
        }
    }
    cleanup_helper(&plan).await
}

async fn cleanup_helper(plan: &Plan) -> Result<()> {
    // Manually invoked recovery/status tests have no registered helper. Do not
    // touch the user's native manager unless this operation owns a definition.
    let directory = operation_dir(&plan.home, plan.id);
    let Ok(platform) = crate::native_service::NativeServicePlatform::current() else {
        return Ok(());
    };
    let mut config = NativeServiceConfig::new(plan.home.clone(), directory.join("helper"))?;
    config.service_config_dir = plan.service_config_dir.clone();
    config.user_home = plan.user_home.clone();
    let registered = match platform {
        crate::native_service::NativeServicePlatform::Macos => directory.join("updater.plist"),
        crate::native_service::NativeServicePlatform::Linux => {
            config.service_config_dir.join(format!(
                "{}.update-{}.service",
                crate::native_service::SERVICE_LABEL,
                plan.id
            ))
        }
    };
    if !tokio::fs::try_exists(registered).await? {
        return Ok(());
    }
    let id = plan.id;
    tokio::task::spawn_blocking(move || {
        NativeServiceManager::new(config)?.unregister_updater(&directory, id)
    })
    .await?
}

#[derive(Clone)]
pub(crate) struct RepairBackup {
    pub(crate) directory: PathBuf,
    config: NativeServiceConfig,
    definition: Vec<u8>,
    network: NetworkSettings,
    enabled: bool,
    running: bool,
    prior_executable: Option<PathBuf>,
}

pub(crate) fn begin_repair_backup(manager: &NativeServiceManager) -> Result<Option<RepairBackup>> {
    if !manager.home().is_dir()
        || !gents_store::local::home::default_data_dir(manager.home()).is_dir()
    {
        return Ok(None);
    }
    let definition = manager.installed_definition()?;
    let status = manager.status()?;
    let config = manager.config().clone();
    let network = NetworkSettings::read(&config.home)?;
    let root = config
        .home
        .join(gents_store::local::home::SERVICE_UPDATES_DIR_NAME);
    files::private_dir(&root)?;
    files::check_backup_capacity(&config.home, &root)?;
    let directory = root.join(format!("repair-{}", uuid::Uuid::new_v4()));
    files::private_dir(&directory)?;
    files::backup_snapshot(
        &config.home,
        &definition,
        &network,
        &directory,
        "before-stop",
    )?;
    let original = manager.definition_executable(&definition)?;
    let prior_executable = if original.is_file() {
        let saved = directory.join("prior-gents");
        files::copy_executable(&original, &saved)?;
        Some(saved)
    } else {
        None
    };
    Ok(Some(RepairBackup {
        directory,
        config,
        definition,
        network,
        enabled: status.enabled,
        running: status.running,
        prior_executable,
    }))
}

pub(crate) async fn finish_repair_backup(backup: Option<&RepairBackup>) -> Result<()> {
    let Some(backup) = backup.cloned() else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || {
        let _store = gents_store::local::home::lock_home_store(&backup.config.home)?;
        files::backup_snapshot(
            &backup.config.home,
            &backup.definition,
            &backup.network,
            &backup.directory,
            "backup",
        )
    })
    .await?
}

pub(crate) async fn restore_repair_service(
    backup: &RepairBackup,
    target: &NativeServiceConfig,
) -> Result<()> {
    let backup = backup.clone();
    let target_to_stop = target.clone();
    let before = backup.definition.clone();
    let expected_running = backup.running;
    let prior_config = backup.config.clone();
    let stopped = tokio::task::spawn_blocking(move || -> Result<_> {
        let manager = NativeServiceManager::new(target_to_stop.clone())?;
        let path = target_to_stop
            .definition_path(crate::native_service::NativeServicePlatform::current()?);
        match std::fs::read(&path) {
            Ok(current) if current != before => {
                anyhow::ensure!(
                    manager.definition_executable(&current)? == target_to_stop.executable,
                    "service changed outside this repair; recovery refused"
                );
            }
            Ok(_) => {
                let previous = NativeServiceManager::new(prior_config.clone())?;
                if previous.status()?.running == expected_running {
                    return Ok(None);
                }
                if expected_running {
                    previous.start(false)?;
                    return Ok(None);
                }
                let pid = previous.job_pid()?;
                previous.stop(false)?;
                return Ok(Some((pid, prior_config)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let pid = manager.job_pid()?;
        manager.stop(false)?;
        Ok(Some((pid, target_to_stop)))
    })
    .await??;
    let Some((pid, cleanup)) = stopped else {
        return Ok(());
    };
    if let Some(pid) = pid {
        anyhow::ensure!(
            crate::native_service::wait_for_process_exit(pid, Duration::from_secs(20)).await,
            "replacement process did not stop; prior service was not overwritten"
        );
    }
    tokio::time::timeout(
        Duration::from_secs(120),
        gents_store::local::home::wait_for_store_release(
            &backup.config.home,
            &gents_store::local::home::default_data_dir(&backup.config.home),
        ),
    )
    .await
    .context("replacement still owns the store")??;
    tokio::task::spawn_blocking(move || {
        let cleanup = NativeServiceManager::new(cleanup)?;
        let store = gents_store::local::home::lock_home_store(&backup.config.home)?;
        cleanup.uninstall()?;
        let replace = backup.prior_executable.is_some();
        let mut config = backup.config;
        if let Some(executable) = backup.prior_executable {
            config.executable = executable;
        }
        let previous = NativeServiceManager::new(config)?;
        previous.install_carrying(&backup.definition, replace, backup.enabled)?;
        drop(store);
        if backup.running {
            previous.start(false)?;
        }
        Ok(())
    })
    .await?
}

pub(crate) async fn can_update_live(home: &Path) -> Result<bool> {
    use gents_protocol::serve_lifecycle::control::{
        RuntimeOperation, NATIVE_STARTUP_FENCE_CAPABILITY,
    };
    let selected_home = home.to_path_buf();
    let (state, init) = tokio::task::spawn_blocking(move || -> Result<_> {
        Ok((
            read_runtime_state(&selected_home)?,
            read_init_config(&selected_home)?,
        ))
    })
    .await??;
    let (Some(state), Some(init)) = (state, init) else {
        return Ok(false);
    };
    let url = reqwest::Url::parse(&state.graphql)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let observed = match crate::runtime_control::control(
        &client,
        &url,
        &init.node_did,
        RuntimeOperation::Observe,
        None,
    )
    .await
    {
        Ok(Some(observed)) => observed,
        Ok(None) => return Ok(false),
        Err(error) => {
            let unsupported = gents_protocol::error_chain::find_map_source(error.as_ref(), |source| {
                source.downcast_ref::<crate::runtime_control::PreparationFailure>()
                    .map(|failure| failure.reason == gents_protocol::serve_lifecycle::preparation::RuntimePreparationRefusal::Unsupported)
            }).unwrap_or(false);
            if unsupported {
                return Ok(false);
            }
            return Err(error);
        }
    };
    if observed.capabilities.get(NATIVE_STARTUP_FENCE_CAPABILITY) != Some(&1) {
        return Ok(false);
    }
    let config = NativeServiceConfig::new(home.to_path_buf(), std::env::current_exe()?)?;
    let installed = tokio::task::spawn_blocking(move || {
        let manager = NativeServiceManager::new(config)?;
        let definition = manager.installed_definition()?;
        files::digest(&manager.definition_executable(&definition)?)
    })
    .await??;
    if observed.build != installed {
        return Ok(false);
    }
    match check_metadata(Some(home.to_path_buf())).await {
        Ok(_) => Ok(true),
        Err(error)
            if gents_store::local::storage_backend::incompatible_store(
                error.as_ref(),
                &gents_store::local::home::default_data_dir(home),
            )
            .is_some() =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub(crate) async fn replace_for_repair(home: &Path, executable: &Path) -> Result<PathBuf> {
    let candidate = executable.to_path_buf();
    let digest = tokio::task::spawn_blocking(move || files::digest(&candidate)).await??;
    let args = ServiceUpdateArgs {
        home: Some(home.to_path_buf()),
        executable: Some(executable.to_path_buf()),
        sha256: Some(digest),
        operation_id: None,
        network: Default::default(),
        environment_file: None,
        drain_timeout_secs: 300,
    };
    let receipt = submit_to(args, Some(executable.to_path_buf()), &mut tokio::io::sink()).await?;
    let path = receipt.directory.clone();
    let completed = async {
        let mut changes = files::Changes::watch(&path).await?;
        loop {
            let receipt: Receipt = files::read_json_async(&path.join("receipt.json")).await?;
            match receipt.phase {
                Phase::Healthy => return Ok(path.join("backup")),
                Phase::Failed => anyhow::bail!(
                    "managed update failed: {}; inspect operation {}",
                    receipt.message,
                    receipt.operation_id
                ),
                _ => changes.changed().await?,
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(420), completed)
        .await
        .context("managed update is still in progress; inspect service update-status")?
}

#[derive(Debug, thiserror::Error)]
#[error("could not stage the repair executable: {0}")]
pub struct ExecutableStageError(#[source] anyhow::Error);

pub fn stage_repair_executable(
    source: &Path,
    desktop_root: &Path,
) -> std::result::Result<PathBuf, ExecutableStageError> {
    (|| -> Result<PathBuf> {
        let digest = files::digest(source)?;
        let root = desktop_root.join("runtime");
        std::fs::create_dir_all(&root)?;
        let directory = root.join(format!("repair-{digest}"));
        std::fs::create_dir_all(&directory)?;
        let target = directory.join("gents");
        if target.exists() && files::digest(&target)? == digest {
            return Ok(target);
        }
        let staged = tempfile::NamedTempFile::new_in(&directory)?;
        files::copy_executable(source, staged.path())?;
        anyhow::ensure!(
            files::digest(staged.path())? == digest,
            "repair source changed while staging"
        );
        files::validate_native_executable(staged.path())?;
        staged.persist(&target)?;
        std::fs::File::open(&directory)?.sync_all()?;
        Ok(target)
    })()
    .map_err(ExecutableStageError)
}
