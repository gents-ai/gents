//! Constrained, independently supervised native-service replacement. Durable
//! receipts survive runtime retirement; they never stand in for store locks or
//! the runtime's authenticated admission fence.
use crate::cli::{ServiceAction, ServiceUpdateArgs};
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
    home.join(gents::home::SERVICE_UPDATES_DIR_NAME)
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

pub(crate) async fn submit(args: ServiceUpdateArgs) -> Result<()> {
    let home = tokio::fs::canonicalize(resolve_home_dir(args.home.as_deref())).await?;
    let id = args.operation_id.unwrap_or_else(uuid::Uuid::new_v4);
    let (plan, mut receipt, _submission_lock) =
        tokio::task::spawn_blocking(move || stage::prepare(home, id, args)).await??;
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
                "update {} failed; inspect `gents service update-status --home {} {}`",
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
            NativeServiceManager::new(service_config(&copy)?)?
                .register_updater(&operation_dir(&copy.home, copy.id), copy.id)
        })
        .await??;
        if receipt.phase == Phase::Registered {
            wait_for_helper(&plan, Duration::from_secs(20)).await?;
        }
    }
    if receipt.phase == Phase::Registered {
        return acknowledge(&mut receipt, &mut tokio::io::stdout()).await;
    }
    emit(&receipt).await
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

pub(crate) async fn status(home: Option<PathBuf>, id: uuid::Uuid) -> Result<()> {
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
            if let Ok(_helper) = files::lock(&directory.join("helper.lock")) {
                NativeServiceManager::new(service_config(&plan)?)?
                    .register_updater(&directory, id)?;
            }
        }
        Ok(receipt)
    })
    .await??;
    emit(&receipt).await
}

pub(crate) use notices::watch_notices;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) fn executable_digest(path: &Path) -> Result<String> {
    files::digest(path)
}

pub(crate) async fn check(home: Option<PathBuf>) -> Result<()> {
    let Some(home) = home else {
        return emit(&metadata(false)).await;
    };
    let _keychain = crate::process_resources::prevent_keychain_prompts(true)?;
    let (identity, state, settings) = tokio::task::spawn_blocking(move || -> Result<_> {
        gents::storage_backend::reject_legacy_store(&gents::home::default_data_dir(&home))?;
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
    let access = gents::config_client::ConfigAccess::Graphql(
        gents::config_client::GraphqlEndpoint::as_principal(state.graphql, identity.did()),
    );
    let versions = access
        .collection_versions()
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<gents::defra_node::CollectionVersion>, _>>()?;
    gents::migration::check_migration_compatibility(&versions)?;
    emit(&metadata(true)).await
}

fn metadata(schema_checked: bool) -> serde_json::Value {
    serde_json::json!({"handoff_version":1,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "schema_checked":schema_checked,"service_label":crate::native_service::SERVICE_LABEL,
        "systemd_unit":crate::native_service::SYSTEMD_UNIT})
}

pub(crate) async fn run(home: PathBuf, id: uuid::Uuid) -> Result<()> {
    let guarded_home = home.clone();
    tokio::task::spawn_blocking(move || {
        refuse_self_disruption(&guarded_home, &ServiceAction::Restart)
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

pub(crate) fn refuse_self_disruption(home: &Path, action: &ServiceAction) -> Result<()> {
    refuse_origin_disruption(
        home,
        std::env::var_os("GENTS_CALLER_RUNTIME_HOME")
            .map(PathBuf::from)
            .as_deref(),
        action,
    )
}
fn refuse_origin_disruption(
    home: &Path,
    origin: Option<&Path>,
    action: &ServiceAction,
) -> Result<()> {
    if matches!(
        action,
        ServiceAction::Stop { .. }
            | ServiceAction::Restart
            | ServiceAction::Install { .. }
            | ServiceAction::Uninstall
    ) {
        if let Some(origin) = origin {
            if let (Ok(origin), Ok(home)) =
                (std::fs::canonicalize(origin), std::fs::canonicalize(home))
            {
                anyhow::ensure!(origin != home, "This command would disrupt the runtime executing your turn. Use `gents service update` and finish your turn after acceptance; inspect `service update-status` after reconnect. Use external administration for unsupported maintenance.");
            }
        }
    }
    Ok(())
}
