use gents_protocol::operations::install_repair::{
    Blocker, HomeState, RepairOperation, ServiceMove,
};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::native_service::{
    CommandRunner, NativeServiceConfig, NativeServiceManager, StartOutcome,
};

use crate::repair::port::{observe_port, read_initialized_did, PortObservation, ServedIdentity};
use crate::repair::ServiceProgram;
use crate::server_host::ServerConfig;

#[derive(Debug, Clone)]
pub struct ServiceFacts {
    pub definition_home: Option<PathBuf>,
    pub executable_present: bool,
    pub log_current: bool,
    pub job_loaded: bool,
    pub job_running: bool,
    pub job_failed: bool,
    pub port: PortObservation,
    pub home: HomeState,
    pub was_serving: bool,
    pub service_move: ServiceMove,
    pub node_version: Option<NodeVersion>,
    pub foreign_program: bool,
    pub managed_update: bool,
    pub drain_supported: bool,
    pub previous_version: Option<semver::Version>,
    pub served_home_present: bool,
    pub operation: RepairOperation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeVersion {
    Older(Option<semver::Version>),
    Same,
    Newer(semver::Version),
}
pub fn node_version(running: Option<&str>, ours: &semver::Version) -> NodeVersion {
    let Some(running) = running.and_then(|running| semver::Version::parse(running.trim()).ok())
    else {
        return NodeVersion::Older(None);
    };
    match running.cmp_precedence(ours) {
        Ordering::Less => NodeVersion::Older(Some(running)),
        Ordering::Equal => NodeVersion::Same,
        Ordering::Greater => NodeVersion::Newer(running),
    }
}
pub fn served_version(port: &PortObservation) -> Option<NodeVersion> {
    let PortObservation::Gents { payload, .. } = port else {
        return None;
    };
    Some(node_version(
        payload
            .get(gents_protocol::serve_lifecycle::STATUS_VERSION_FIELD)
            .and_then(serde_json::Value::as_str),
        &this_version(),
    ))
}
pub fn bundle_version(program: &Path) -> Option<semver::Version> {
    let bundle = program
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|extension| extension == "app"))?;
    let info: plist::Dictionary = plist::from_file(bundle.join("Contents/Info.plist")).ok()?;
    let named = info.get("CFBundleShortVersionString")?.as_string()?;
    let version = semver::Version::parse(named.trim()).ok()?;
    (version.cmp_precedence(&this_version()) == Ordering::Less).then_some(version)
}

pub fn this_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package versions are semver")
}
pub fn newer_node(facts: &ServiceFacts) -> Option<Blocker> {
    match &facts.node_version {
        Some(NodeVersion::Newer(running)) => Some(Blocker::NewerNode {
            running: running.to_string(),
            ours: this_version().to_string(),
        }),
        _ => None,
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceAction {
    MoveFrom {
        from: PathBuf,
        start_after: bool,
        replace_program: bool,
    },
    StopStale,
    Reinstall {
        start_after: bool,
    },
    SwitchProgram {
        from: Option<semver::Version>,
        answered: bool,
        start_after: bool,
    },
    RefreshLog {
        restart: bool,
    },
    Restart,
    RewriteDefinition,
}

pub fn plan_service(facts: &ServiceFacts, chosen: &Path) -> Vec<ServiceAction> {
    let serves_this_node = matches!(
        facts.port,
        PortObservation::Gents {
            identity: ServedIdentity::Matches,
            ..
        }
    );
    let Some(definition_home) = &facts.definition_home else {
        return if !facts.job_loaded {
            Vec::new()
        } else if serves_this_node {
            vec![ServiceAction::RewriteDefinition]
        } else {
            vec![ServiceAction::StopStale]
        };
    };
    let complete = facts.home.is_complete();
    if definition_home != chosen {
        if facts.served_home_present && facts.operation != RepairOperation::Repoint {
            return Vec::new();
        }
        if facts.service_move == ServiceMove::AfterSetup && !complete {
            return Vec::new();
        }
        return vec![ServiceAction::MoveFrom {
            from: definition_home.clone(),
            start_after: (facts.job_loaded || facts.was_serving) && complete,
            replace_program: facts.foreign_program,
        }];
    }
    if !facts.executable_present {
        return vec![ServiceAction::Reinstall {
            start_after: facts.job_loaded && complete,
        }];
    }
    let switch = match &facts.node_version {
        Some(NodeVersion::Older(from)) if facts.job_running && !facts.managed_update => {
            Some((from.clone(), true))
        }
        None if facts.foreign_program && !facts.job_running => {
            Some((facts.previous_version.clone(), false))
        }
        _ => None,
    };
    if let Some((from, answered)) = switch {
        return vec![ServiceAction::SwitchProgram {
            from,
            answered,
            start_after: (facts.job_loaded || facts.job_failed) && complete,
        }];
    }
    let serves_another_node = matches!(
        facts.port,
        PortObservation::Gents {
            identity: ServedIdentity::Different { .. },
            ..
        }
    );
    let stop_stale = (facts.job_loaded && !complete) || (facts.job_running && serves_another_node);
    let mut actions = Vec::new();
    if stop_stale {
        actions.push(ServiceAction::StopStale);
    }
    if !facts.log_current {
        actions.push(ServiceAction::RefreshLog {
            restart: facts.job_loaded && complete && !serves_this_node && !stop_stale,
        });
    }
    if facts.job_failed && !facts.job_loaded && complete && !serves_this_node {
        actions.push(ServiceAction::Restart);
    }
    actions
}

pub async fn gather_facts(
    manager: &NativeServiceManager,
    chosen: &Path,
    server: &ServerConfig,
    home: HomeState,
    program: &ServiceProgram,
) -> Result<ServiceFacts> {
    let config = manager.config().clone();
    let chosen_home = chosen.to_path_buf();
    let (mut facts, installed) = tokio::task::spawn_blocking(move || -> Result<_> {
        let manager = NativeServiceManager::new(config.clone())?;
        let installed_home = manager.installed_home()?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let owned = manager.definition_owned_by_home()?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let owned = false;
        let mut facts = ServiceFacts {
            definition_home: if owned {
                Some(chosen_home)
            } else {
                installed_home.clone()
            },
            executable_present: true,
            log_current: true,
            job_loaded: false,
            job_running: false,
            job_failed: false,
            port: PortObservation::NotListening,
            home,
            was_serving: false,
            service_move: ServiceMove::BeforeSetup,
            node_version: None,
            foreign_program: false,
            managed_update: false,
            drain_supported: false,
            previous_version: None,
            served_home_present: false,
            operation: RepairOperation::Run,
        };
        let Some(installed_home) = installed_home else {
            facts.job_loaded = manager.job_loaded()?;
            return Ok((facts, None));
        };
        let owner = NativeServiceManager::new(NativeServiceConfig {
            home: installed_home,
            ..config
        })?;
        let status = owner.status()?;
        facts.job_loaded = status.job_loaded;
        facts.job_running = status.running;
        facts.job_failed = status.failed;
        facts.log_current = owner.definition_log_current()?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let installed = {
            let installed = owner.installed_executable_path()?;
            facts.executable_present = installed.exists();
            facts.managed_update = yields_to_update(&owner)?;
            Some(installed)
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let installed = None;
        Ok((facts, installed))
    })
    .await??;
    let initialized_did = read_initialized_did(chosen).await;
    facts.port = observe_port(server, initialized_did.as_deref()).await;
    facts.foreign_program = match installed.clone() {
        Some(installed) if compares_program(&facts, chosen) => {
            program_is_foreign(installed, program).await
        }
        _ => false,
    };
    if let Some(installed) = installed.filter(|_| facts.foreign_program) {
        facts.previous_version = tokio::task::spawn_blocking(move || bundle_version(&installed))
            .await
            .ok()
            .flatten();
    }
    if facts.job_running
        && facts.definition_home.as_deref() == Some(chosen)
        && matches!(served_version(&facts.port), Some(NodeVersion::Older(_)))
    {
        facts.drain_supported = crate::service_update::can_update_live(chosen).await?;
        facts.managed_update = on_manager(manager.config(), |manager| {
            Ok(manager.pending_update()?.is_some())
        })
        .await?;
    }
    Ok(facts)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn yields_to_update<R>(owner: &NativeServiceManager<R>) -> Result<bool>
where
    R: CommandRunner,
{
    if owner.has_managed_executable()? {
        return Ok(true);
    }
    match owner.pending_update() {
        Ok(pending) => Ok(pending.is_some()),
        Err(error) => {
            tracing::warn!(
                error = format!("{error:#}"),
                "service update record unreadable; leaving the program to the update"
            );
            Ok(true)
        }
    }
}
fn compares_program(facts: &ServiceFacts, chosen: &Path) -> bool {
    match &facts.definition_home {
        _ if facts.managed_update => false,
        Some(home) if home == chosen => {
            facts.port == PortObservation::NotListening
                && !facts.job_running
                && facts.executable_present
        }
        Some(_) => true,
        None => false,
    }
}
async fn program_is_foreign(installed: PathBuf, program: &ServiceProgram) -> bool {
    let program = program.clone();
    tokio::task::spawn_blocking(move || {
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        program.resolve().is_ok_and(|ours| {
            if canonical(&installed) == canonical(&ours) {
                return false;
            }
            match (
                crate::service_update::executable_digest(&installed),
                crate::service_update::executable_digest(&ours),
            ) {
                (Ok(installed), Ok(ours)) => installed != ours,
                _ => true,
            }
        })
    })
    .await
    .unwrap_or(false)
}
pub struct AppliedRepair {
    pub backup_path: PathBuf,
    backup: Option<crate::service_update::RepairBackup>,
    target: NativeServiceConfig,
}

pub async fn recover(applied: &AppliedRepair) -> Result<(), Blocker> {
    let Some(backup) = &applied.backup else {
        return Ok(());
    };
    crate::service_update::restore_repair_service(backup, &applied.target)
        .await
        .map_err(|error| Blocker::ServiceUpdateFailed {
            reason: error.to_string(),
        })
}

pub async fn apply(
    action: &ServiceAction,
    manager: &NativeServiceManager,
    reinstall_installs: bool,
    use_managed_update: bool,
) -> Result<Option<AppliedRepair>, Blocker> {
    if use_managed_update && matches!(action, ServiceAction::SwitchProgram { .. }) {
        let backup_path =
            crate::service_update::replace_for_repair(manager.home(), &manager.config().executable)
                .await
                .map_err(|error| Blocker::ServiceUpdateFailed {
                    reason: error.to_string(),
                })?;
        return Ok(Some(AppliedRepair {
            backup_path,
            backup: None,
            target: manager.config().clone(),
        }));
    }
    let mut previous = manager.config().clone();
    if let ServiceAction::MoveFrom { from, .. } = action {
        previous.home = from.clone();
    }
    let changes_program = matches!(
        action,
        ServiceAction::MoveFrom { .. }
            | ServiceAction::Reinstall { .. }
            | ServiceAction::SwitchProgram { .. }
    );
    let backup = if changes_program {
        on_manager(&previous, crate::service_update::begin_repair_backup).await?
    } else {
        None
    };
    let applied = backup.map(|backup| AppliedRepair {
        backup_path: backup.directory.clone(),
        backup: Some(backup),
        target: manager.config().clone(),
    });
    let result = apply_inner(
        action,
        manager,
        reinstall_installs,
        applied.as_ref().and_then(|a| a.backup.as_ref()),
    )
    .await;
    if let Err(error) = result {
        if let Some(applied) = &applied {
            if let Err(recovery) = recover(applied).await {
                return Err(Blocker::ServiceUpdateFailed {
                    reason: format!("{error}; prior service recovery failed: {recovery}"),
                });
            }
        }
        return Err(error);
    }
    Ok(applied)
}

async fn apply_inner(
    action: &ServiceAction,
    manager: &NativeServiceManager,
    reinstall_installs: bool,
    backup: Option<&crate::service_update::RepairBackup>,
) -> Result<(), Blocker> {
    let config = manager.config();
    let exposure = backup.map(|backup| backup.directory.clone());
    match action {
        ServiceAction::MoveFrom {
            from,
            start_after,
            replace_program,
        } => {
            let replace_program = *replace_program;
            let previous = NativeServiceConfig {
                home: from.clone(),
                ..config.clone()
            };
            let chosen = config.clone();
            let (carried, install) = on_manager(&previous, move |previous| {
                let target = chosen
                    .home
                    .is_dir()
                    .then(|| NativeServiceManager::new(chosen))
                    .transpose()?;
                let carried = stop_for_handover(previous, target.as_ref(), replace_program)?;
                Ok((carried, target.is_some()))
            })
            .await?;
            wait_for_exit(carried.pid).await?;
            crate::service_update::finish_repair_backup(backup)
                .await
                .map_err(|error| Blocker::ServiceUpdateFailed {
                    reason: error.to_string(),
                })?;
            on_manager(&previous, NativeServiceManager::uninstall).await?;
            if install {
                on_manager(config, move |manager| {
                    crate::service_update::expose_repair_candidate(exposure.as_deref())?;
                    manager.install_carrying(&carried.definition, replace_program, carried.enabled)
                })
                .await?;
                if *start_after {
                    start_after_stop(config).await?;
                }
            }
        }
        ServiceAction::StopStale => {
            let pid = on_manager(config, |manager| {
                let pid = manager.job_pid()?;
                manager.stop(false)?;
                Ok(pid)
            })
            .await?;
            wait_for_exit(pid).await?;
        }
        ServiceAction::Reinstall { start_after }
        | ServiceAction::SwitchProgram { start_after, .. } => {
            let install = reinstall_installs;
            let carried =
                on_manager(config, move |manager| stop_for_reinstall(manager, install)).await?;
            wait_for_exit(carried.pid).await?;
            crate::service_update::finish_repair_backup(backup)
                .await
                .map_err(|error| Blocker::ServiceUpdateFailed {
                    reason: error.to_string(),
                })?;
            on_manager(config, move |manager| {
                manager.uninstall()?;
                if install {
                    crate::service_update::expose_repair_candidate(exposure.as_deref())?;
                    manager.install_carrying(&carried.definition, true, carried.enabled)?;
                }
                Ok(())
            })
            .await?;
            if install && *start_after {
                start_after_stop(config).await?;
            }
        }
        ServiceAction::Restart => start_after_stop(config).await?,
        ServiceAction::RewriteDefinition => {
            on_manager(config, NativeServiceManager::install).await?
        }
        ServiceAction::RefreshLog { restart } => {
            let restart = *restart;
            let pid = on_manager(config, move |manager| {
                manager.refresh_definition_file()?;
                if !restart {
                    return Ok(None);
                }
                let pid = manager.job_pid()?;
                manager.stop(false)?;
                Ok(pid)
            })
            .await?;
            if restart {
                wait_for_exit(pid).await?;
                start_after_stop(config).await?;
            }
        }
    }
    Ok(())
}
struct Carried {
    definition: Vec<u8>,
    enabled: bool,
    pid: Option<u32>,
}
fn stop_for_handover<R>(
    old: &NativeServiceManager<R>,
    target: Option<&NativeServiceManager<R>>,
    replace_executable: bool,
) -> Result<Carried>
where
    R: CommandRunner,
{
    let carried = Carried {
        definition: old.installed_definition()?,
        enabled: old.status()?.enabled,
        pid: old.job_pid()?,
    };
    if let Some(target) = target {
        target.check_carrying(&carried.definition, replace_executable)?;
    }
    old.stop(false)?;
    Ok(carried)
}
fn stop_for_reinstall<R>(manager: &NativeServiceManager<R>, install: bool) -> Result<Carried>
where
    R: CommandRunner,
{
    stop_for_handover(manager, install.then_some(manager), true)
}
pub(super) async fn on_manager<T, F>(config: &NativeServiceConfig, work: F) -> Result<T, Blocker>
where
    T: Send + 'static,
    F: FnOnce(&NativeServiceManager) -> Result<T> + Send + 'static,
{
    let config = config.clone();
    tokio::task::spawn_blocking(move || work(&NativeServiceManager::new(config)?))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result)
        .map_err(|error| Blocker::ServiceUpdateFailed {
            reason: format!("{error:#}"),
        })
}
async fn start_after_stop(config: &NativeServiceConfig) -> Result<(), Blocker> {
    start_job(config).await
}

pub(super) async fn start_job(config: &NativeServiceConfig) -> Result<(), Blocker> {
    match on_manager(config, |manager| manager.start_or_approval(false)).await? {
        StartOutcome::Started => Ok(()),
        StartOutcome::ApprovalPending { detail } => {
            Err(Blocker::BackgroundApprovalPending { detail })
        }
    }
}

async fn wait_for_exit(pid: Option<u32>) -> Result<(), Blocker> {
    #[cfg(unix)]
    if let Some(pid) = pid {
        if !crate::native_service::wait_for_process_exit(pid, Duration::from_secs(20)).await {
            return Err(Blocker::OldNodeStillRunning { pid });
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    Ok(())
}
