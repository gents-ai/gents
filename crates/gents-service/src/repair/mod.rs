use gents_protocol::operations::install_repair::{
    Blocker, Finding, HomeChoice, HomeDecision, HomeState, OlderData, RepairAnswer,
    RepairOperation, RepairOutcome, RepairReceipt, RepairStep, ServiceMove, StoreVerdict,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gents_protocol::operations::install_repair::{IncompatibleStore, IncompatibleStoreKind};
use gents_store::local::file_lock::FileLock;
use gents_store::local::home::default_data_dir;
use gents_store::local::storage_backend::incompatible_store_kind;
use tokio::time::Instant;

use crate::home_state::{default_home_ignoring_env, read_runtime_state};
use crate::native_service::{
    NativeServiceConfig, NativeServiceManager, NativeServicePlatform, ServiceExit, ServiceHolder,
    ServiceRefusal,
};
use crate::repair::crash_loop::{job_sample, CrashLoopWatch};
use crate::repair::home::{
    aside_label, classify_home, held_by_another, set_aside_home_as, store_check,
    unless_being_set_up,
};
use crate::repair::port::{
    observe_port, port_holder, read_initialized_did, served_identity, PortObservation,
    ServedIdentity,
};
use crate::repair::reason::{blocker_for_exit, read_last_error};
use gents_protocol::graphql::{graphql_endpoint_available, GraphqlRequestOptions};

use crate::repair::service::{
    apply, gather_facts, newer_node, on_manager, plan_before_setup, plan_service, served_version,
    start_job, ServiceAction, ServiceFacts,
};
use crate::server_host::ServerConfig;

pub mod client;
pub mod crash_loop;
pub mod home;
pub mod planning;
pub mod port;
pub mod reason;
pub mod service;
pub trait Progress: Send {
    fn finding(&mut self, finding: &Finding);
    fn step(&mut self, index: usize, total: usize, step: RepairStep);
}
pub const ENDPOINT_WAIT: Duration = Duration::from_secs(60);
const SAMPLE_EVERY: Duration = Duration::from_secs(1);
const POLL_EVERY: Duration = Duration::from_millis(100);
#[derive(Clone)]
pub struct ServiceProgram {
    resolver: Arc<dyn Fn() -> Result<PathBuf, Blocker> + Send + Sync>,
}

impl ServiceProgram {
    pub fn new<F>(resolver: F) -> Self
    where
        F: Fn() -> Result<PathBuf, Blocker> + Send + Sync + 'static,
    {
        Self {
            resolver: Arc::new(resolver),
        }
    }

    fn resolve(&self) -> Result<PathBuf, Blocker> {
        (self.resolver)()
    }
}
async fn resolve(program: &ServiceProgram) -> Result<PathBuf, Blocker> {
    let program = program.clone();
    tokio::task::spawn_blocking(move || program.resolve())
        .await
        .unwrap_or_else(|join| {
            Err(Blocker::ServiceUpdateFailed {
                reason: join.to_string(),
            })
        })
}
/// What the user can be asked, and what they answered.
#[derive(Debug, Clone, Copy)]
struct Asked {
    interactive: bool,
    answer: RepairAnswer,
}

const UNASKED: Asked = Asked {
    interactive: false,
    answer: RepairAnswer::None,
};

async fn prepare(
    choice: &HomeChoice,
    program: &ServiceProgram,
    service_move: ServiceMove,
    operation: RepairOperation,
    asked: Asked,
    progress: &mut dyn Progress,
) -> Result<RepairReceipt> {
    prepare_with_lock(
        &repair_lock_path()?,
        local_service(&choice.home, &current_executable()?)?,
        &ServerConfig::standard(choice.home.clone()),
        choice,
        false,
        program,
        service_move,
        operation,
        asked,
        progress,
    )
    .await
}
async fn restore(choice: &HomeChoice, progress: &mut dyn Progress) -> Result<RepairReceipt> {
    prepare_with_lock(
        &repair_lock_path()?,
        local_service(&choice.home, &current_executable()?)?,
        &ServerConfig::standard(choice.home.clone()),
        choice,
        true,
        &ServiceProgram::new(current_program),
        ServiceMove::AfterSetup,
        RepairOperation::Repoint,
        UNASKED,
        progress,
    )
    .await
}
#[expect(
    clippy::too_many_arguments,
    reason = "the lock and status server are injected so tests never touch the user's own"
)]
async fn prepare_with_lock(
    lock: &Path,
    manager: Option<NativeServiceManager>,
    server: &ServerConfig,
    choice: &HomeChoice,
    was_serving: bool,
    program: &ServiceProgram,
    service_move: ServiceMove,
    operation: RepairOperation,
    asked: Asked,
    progress: &mut dyn Progress,
) -> Result<RepairReceipt> {
    let _lock = hold_lock(lock).await?;
    let (last_exit, running) = match &manager {
        Some(manager) => {
            let sampled = on_manager(manager.config(), |manager| {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                let owned = manager.definition_owned_by_home()?;
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                let owned = false;
                if !owned {
                    return Ok((None, false));
                }
                Ok((manager.last_exit()?, manager.status()?.running))
            })
            .await;
            match sampled {
                Ok(sampled) => sampled,
                Err(blocker) => return Ok(blocked(blocker)),
            }
        }
        None => (None, false),
    };
    let last_refusal = last_exit.as_ref().and_then(ServiceExit::refusal);
    let mut refusal = store_check(&choice.home).await;
    if refusal.is_none() && !running {
        refusal = older_refused(&choice.home, last_exit.as_ref()).await;
    }
    let verdict = refusal
        .as_ref()
        .map_or(StoreVerdict::Fine, |store| StoreVerdict::of(store.kind));
    let chosen = choice.home.clone();
    let home =
        match tokio::task::spawn_blocking(move || classify_home(&chosen, last_refusal, verdict))
            .await?
        {
            Ok(home) => home,
            Err(blocker) => return Ok(blocked(blocker)),
        };
    let home = if home == HomeState::Interrupted {
        let serving = node_serves(manager.as_ref(), &choice.home).await;
        let chosen = choice.home.clone();
        match tokio::task::spawn_blocking(move || unless_being_set_up(&chosen, home, serving))
            .await?
        {
            Ok(home) => home,
            Err(blocker) => return Ok(blocked(blocker)),
        }
    } else {
        home
    };
    let mut facts = match &manager {
        Some(manager) => gather_facts(manager, &choice.home, server, home, program).await?,
        None => ServiceFacts {
            definition_home: None,
            executable_present: true,
            log_current: true,
            job_loaded: false,
            job_running: false,
            job_failed: false,
            port: observe_port(server, read_initialized_did(&choice.home).await.as_deref()).await,
            home,
            was_serving: false,
            service_move,
            node_version: None,
            foreign_program: false,
            managed_update: false,
            drain_supported: false,
            previous_version: None,
            served_home_present: false,
            operation,
            holder: ServiceHolder::Free,
        },
    };
    facts.node_version = served_version(&facts.port);
    facts.was_serving = was_serving;
    facts.service_move = service_move;
    facts.operation = operation;
    facts.served_home_present =
        served_home_present(facts.definition_home.as_deref(), &choice.home).await;
    let busy = !refuses_home(home) && home_busy(manager.as_ref(), &facts, &choice.home).await;
    let serving_home =
        serving_home(facts.definition_home.as_deref(), &facts.port, &choice.home).await;
    let mut report = settle(
        choice,
        &facts,
        busy,
        asked,
        refusal,
        manager.as_ref(),
        serving_home.as_deref(),
        program,
        progress,
    )
    .await?;
    let moved = report
        .repaired
        .iter()
        .any(|finding| matches!(finding, Finding::ServiceForAnotherFolder { .. }));
    if moved || matches!(report.outcome, RepairOutcome::Decide(_)) {
        report.serving_home = serving_home;
    }
    Ok(report)
}
async fn served_home_present(definition_home: Option<&Path>, chosen: &Path) -> bool {
    let Some(served) = definition_home.filter(|home| *home != chosen) else {
        return false;
    };
    let served = served.to_path_buf();
    tokio::task::spawn_blocking(move || {
        !matches!(
            classify_home(&served, None, StoreVerdict::Fine),
            Ok(HomeState::Absent)
        )
    })
    .await
    .unwrap_or(true)
}
/// A home the plan refuses as it is: probing its store lock would write the
/// lock file, and nothing in such a home is touched.
fn refuses_home(home: HomeState) -> bool {
    matches!(
        home,
        HomeState::OlderStore | HomeState::NewerStore | HomeState::InsecureKey
    )
}
async fn home_busy(
    manager: Option<&NativeServiceManager>,
    facts: &ServiceFacts,
    chosen: &Path,
) -> bool {
    let service_job = match manager {
        Some(manager) if facts.job_running && facts.definition_home.as_deref() == Some(chosen) => {
            Some(
                on_manager(manager.config(), |manager| manager.job_pid())
                    .await
                    .ok()
                    .flatten(),
            )
        }
        _ => None,
    };
    let chosen = chosen.to_path_buf();
    tokio::task::spawn_blocking(move || held_by_another(&chosen, service_job))
        .await
        .unwrap_or(false)
}
async fn node_serves(manager: Option<&NativeServiceManager>, home: &Path) -> bool {
    let job_running = match manager {
        Some(manager) => on_manager(manager.config(), |manager| {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            let owned = manager.definition_owned_by_home()?;
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            let owned = false;
            Ok(owned && manager.status()?.running)
        })
        .await
        .unwrap_or(false),
        None => false,
    };
    job_running || answering_graphql(home).await.ok().flatten().is_some()
}
async fn serving_home(
    definition_home: Option<&Path>,
    port: &PortObservation,
    chosen: &Path,
) -> Option<PathBuf> {
    let PortObservation::Gents { payload, .. } = port else {
        return None;
    };
    let home = definition_home.filter(|home| *home != chosen)?;
    let initialized = read_initialized_did(home).await;
    (served_identity(initialized.as_deref(), payload) == ServedIdentity::Matches)
        .then(|| home.to_path_buf())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the observed facts, the user's answer and the injected manager each decide part of the plan"
)]
async fn settle(
    choice: &HomeChoice,
    facts: &ServiceFacts,
    busy: bool,
    asked: Asked,
    refusal: Option<IncompatibleStore>,
    manager: Option<&NativeServiceManager>,
    serving_home: Option<&Path>,
    program: &ServiceProgram,
    progress: &mut dyn Progress,
) -> Result<RepairReceipt> {
    use gents_protocol::operations::install_repair::RepairPlanBlocker;
    let plan = planning::plan(&observe(choice, facts, busy, asked));
    match plan.blocker {
        RepairPlanBlocker::None => {}
        RepairPlanBlocker::Ask
        | RepairPlanBlocker::Unattended
        | RepairPlanBlocker::UnfinishedSetup => {
            return Ok(RepairReceipt {
                client: None,
                repaired: Vec::new(),
                outcome: RepairOutcome::Decide(
                    decision(&choice.home, facts.home, serving_home).await?,
                ),
                serving_home: serving_home.map(Path::to_path_buf),
                older_data: None,
                unfinished_setup: false,
            });
        }
        RepairPlanBlocker::OlderStore | RepairPlanBlocker::InsecureKey => {
            let store = refusal.unwrap_or_else(|| IncompatibleStore {
                kind: match plan.blocker {
                    RepairPlanBlocker::InsecureKey => IncompatibleStoreKind::InsecureKey,
                    _ => IncompatibleStoreKind::OtherVersionSchema,
                },
                data_path: default_data_dir(&choice.home),
            });
            return Ok(blocked(Blocker::StoreRefused {
                kind: store.kind,
                message: store.to_string(),
            }));
        }
        RepairPlanBlocker::NewerVersion => {
            return Ok(blocked(newer_node(facts).unwrap_or_else(|| {
                let store = refusal.unwrap_or_else(|| IncompatibleStore {
                    kind: IncompatibleStoreKind::ForeignVersion,
                    data_path: default_data_dir(&choice.home),
                });
                Blocker::StoreRefused {
                    kind: store.kind,
                    message: store.to_string(),
                }
            })))
        }
        _ => {
            return Ok(blocked(Blocker::HomeBusy {
                home: choice.home.clone(),
            }))
        }
    }
    let archive = plan.steps.contains(&RepairStep::SetAsideFolder);
    let continuing = planning::takes_home(&plan, facts.home) && !archive;
    let actions = service_actions(&plan, facts, &choice.home);
    let answered = answered_switch(&actions);
    let started_from = answered
        .clone()
        .filter(|(_, started)| *started)
        .map(|(from, _)| from);
    let replacing = replaced_service(choice, &actions);
    let replaced = replacing.map(|_| OlderData {
        from: facts.previous_version.clone(),
    });
    let mut steps: Vec<RepairStep> = actions.iter().map(step_for).collect();
    if started_from.is_some() || replacing == Some(true) {
        steps.push(RepairStep::WaitForNode);
    }
    if archive {
        steps.push(RepairStep::SetAsideFolder);
    }
    let mut repairs = Repairs::new(progress, steps.len());
    for finding in actions
        .iter()
        .filter_map(|action| finding_for(action, facts))
    {
        repairs.found(finding);
    }
    let mut applied_services = Vec::new();
    for action in &actions {
        repairs.step(step_for(action));
        let manager = manager.context("a service repair needs the service manager")?;
        let installs_program = choice.home.is_dir()
            && match action {
                ServiceAction::Reinstall { .. } | ServiceAction::SwitchProgram { .. } => true,
                ServiceAction::MoveFrom {
                    replace_program, ..
                } => *replace_program,
                _ => false,
            };
        let installing;
        let manager = if installs_program {
            match resolve(program).await {
                Ok(executable) => {
                    installing = NativeServiceManager::new(NativeServiceConfig {
                        executable,
                        ..manager.config().clone()
                    })?;
                    &installing
                }
                Err(blocker) => return Ok(repairs.report(RepairOutcome::Blocked(blocker))),
            }
        } else {
            manager
        };
        match apply(action, manager, installs_program, facts.drain_supported).await {
            Ok(Some(applied)) => {
                repairs.found(Finding::BackupSaved {
                    kept_at: applied.backup_path.clone(),
                });
                applied_services.push(applied);
            }
            Ok(None) => {}
            Err(blocker) => return Ok(repairs.report(RepairOutcome::Blocked(blocker))),
        }
        if let ServiceAction::SwitchProgram { from, .. } = action {
            repairs.found(Finding::MovedToThisVersion { from: from.clone() });
        }
    }
    let mut outcome = if archive {
        repairs.step(RepairStep::SetAsideFolder);
        let label = match facts.home {
            HomeState::UnfinishedSetup => "unfinished".to_owned(),
            _ => aside_label(None),
        };
        match set_aside_home_as(&choice.home, label).await {
            Ok(kept_at) => {
                repairs.found(Finding::BrokenHome {
                    why: facts.home,
                    kept_at,
                });
                RepairOutcome::NeedsSetup
            }
            Err(blocker) => RepairOutcome::Blocked(blocker),
        }
    } else if continuing {
        RepairOutcome::NeedsSetup
    } else {
        match (&facts.home, started_from, manager) {
            (home, Some(_), Some(manager)) if home.is_complete() => {
                repairs.step(RepairStep::WaitForNode);
                opens_after_switch(&choice.home, manager)
                    .await
                    .unwrap_or_else(repair_error)
            }
            (home, None, Some(manager)) if home.is_complete() && replacing == Some(true) => {
                repairs.step(RepairStep::WaitForNode);
                let serving = serving_home.filter(|_| choice.explicit);
                opens_after_replacing(&choice.home, manager, serving)
                    .await
                    .unwrap_or_else(repair_error)
            }
            (HomeState::Absent, ..) => RepairOutcome::NeedsSetup,
            _ => RepairOutcome::Ready,
        }
    };
    if matches!(outcome, RepairOutcome::Blocked(_)) {
        for applied in applied_services.iter().rev() {
            repairs.extra_step(RepairStep::Rollback);
            if let Err(error) = service::recover(applied).await {
                outcome = RepairOutcome::Blocked(error);
                break;
            }
        }
    }
    let mut report = repairs.report(outcome);
    report.older_data = replaced.or_else(|| {
        (!choice.explicit).then(|| OlderData {
            from: answered.and_then(|(from, _)| from),
        })
    });
    report.unfinished_setup = facts.home == HomeState::UnfinishedSetup && !continuing && !archive;
    Ok(report)
}

fn observe(
    choice: &HomeChoice,
    facts: &ServiceFacts,
    busy: bool,
    asked: Asked,
) -> gents_protocol::operations::install_repair::RepairObservation {
    use gents_protocol::operations::install_repair::{RepairObservation, RepairVersion};
    RepairObservation {
        home: facts.home,
        version: match &facts.node_version {
            Some(service::NodeVersion::Older(_)) => RepairVersion::Older,
            Some(service::NodeVersion::Same) => RepairVersion::Same,
            Some(service::NodeVersion::Newer(_)) => RepairVersion::Newer,
            None => RepairVersion::Unknown,
        },
        interactive: asked.interactive,
        answer: asked.answer,
        busy,
        keychain_locked: false,
        foreign_process: false,
        readable: true,
        service_present: facts.definition_home.is_some(),
        service_running: facts.job_running,
        service_program_present: facts.executable_present,
        service_same_home: facts.definition_home.as_deref() == Some(choice.home.as_path()),
        managed_update: facts.managed_update,
        drain_supported: facts.drain_supported,
        served_home_present: facts.served_home_present,
        operation: facts.operation,
    }
}

/// `InstallRepair.settleServicePlan`: every change the repair makes to the
/// service is decided here from the plan.
fn service_actions(
    plan: &gents_protocol::operations::install_repair::RepairPlan,
    facts: &ServiceFacts,
    chosen: &Path,
) -> Vec<ServiceAction> {
    use gents_protocol::operations::install_repair::RepairPlanBlocker;
    if plan.blocker != RepairPlanBlocker::None {
        Vec::new()
    } else if planning::takes_home(plan, facts.home) {
        plan_before_setup(facts, chosen)
    } else {
        plan_service(facts, chosen)
    }
}

fn repair_error(error: anyhow::Error) -> RepairOutcome {
    RepairOutcome::Blocked(Blocker::ServiceUpdateFailed {
        reason: gents_protocol::error_chain::ErrorChain(error.as_ref()).to_string(),
    })
}

fn replaced_service(choice: &HomeChoice, actions: &[ServiceAction]) -> Option<bool> {
    if choice.explicit {
        return None;
    }
    actions.iter().find_map(|action| match action {
        ServiceAction::SwitchProgram {
            answered: false,
            start_after,
            ..
        }
        | ServiceAction::MoveFrom { start_after, .. }
        | ServiceAction::Reinstall { start_after } => Some(*start_after),
        _ => None,
    })
}
async fn opens_after_replacing(
    home: &Path,
    manager: &NativeServiceManager,
    serving_home: Option<&Path>,
) -> Result<RepairOutcome> {
    let since = Since {
        platform: NativeServicePlatform::current()?,
        runs: None,
        started: true,
    };
    let waited = wait_for_node(home, manager, since, serving_home).await?;
    Ok(after_replacing(waited))
}

fn after_replacing(waited: RepairOutcome) -> RepairOutcome {
    match waited {
        RepairOutcome::Answering(_) => RepairOutcome::Ready,
        outcome => outcome,
    }
}
fn answered_switch(actions: &[ServiceAction]) -> Option<(Option<semver::Version>, bool)> {
    actions.iter().find_map(|action| match action {
        ServiceAction::SwitchProgram {
            from,
            answered: true,
            start_after,
        } => Some((from.clone(), *start_after)),
        _ => None,
    })
}
async fn opens_after_switch(home: &Path, manager: &NativeServiceManager) -> Result<RepairOutcome> {
    let since = Since {
        platform: NativeServicePlatform::current()?,
        runs: None,
        started: true,
    };
    Ok(after_replacing(
        wait_for_node(home, manager, since, None).await?,
    ))
}

async fn decision(home: &Path, why: HomeState, serving: Option<&Path>) -> Result<HomeDecision> {
    Ok(HomeDecision {
        home: home.to_path_buf(),
        why,
        default_home: tokio::task::spawn_blocking(default_home_ignoring_env).await??,
        serving_home: serving.map(Path::to_path_buf),
    })
}

fn step_for(action: &ServiceAction) -> RepairStep {
    match action {
        ServiceAction::StopStale => RepairStep::StopOldNode,
        ServiceAction::Restart => RepairStep::StartNode,
        ServiceAction::MoveFrom { .. }
        | ServiceAction::Reinstall { .. }
        | ServiceAction::SwitchProgram { .. }
        | ServiceAction::RefreshLog { .. }
        | ServiceAction::RewriteDefinition => RepairStep::UpdateService,
    }
}

/// What a step repairs, said only when it is a repair: a node stopped so the
/// user's chosen setup can run is not one.
fn finding_for(action: &ServiceAction, facts: &ServiceFacts) -> Option<Finding> {
    match action {
        ServiceAction::MoveFrom { from, .. } => {
            Some(Finding::ServiceForAnotherFolder { from: from.clone() })
        }
        ServiceAction::StopStale if facts.definition_home.is_none() => {
            Some(Finding::ServiceJobWithoutDefinition)
        }
        ServiceAction::StopStale if facts.home.is_complete() => Some(Finding::StaleNode),
        ServiceAction::Reinstall { .. } => Some(Finding::ServiceProgramMissing),
        ServiceAction::RewriteDefinition => Some(Finding::ServiceDefinitionMissing),
        ServiceAction::StopStale
        | ServiceAction::SwitchProgram { .. }
        | ServiceAction::RefreshLog { .. }
        | ServiceAction::Restart => None,
    }
}
async fn start(
    home: &Path,
    serving_home: Option<&Path>,
    progress: &mut dyn Progress,
) -> Result<RepairReceipt> {
    let _lock = hold_lock(&repair_lock_path()?).await?;
    let mut repairs = Repairs::new(progress, 2);
    if let Some(graphql) = answering_graphql(home).await? {
        let server = answering_server(home, &graphql)?;
        let observed = observe_port(&server, read_initialized_did(home).await.as_deref()).await;
        let outcome = match hand_started(server.http_port, observed) {
            Some(blocker) => RepairOutcome::Blocked(service_serving_another(home).await.map_or(
                blocker,
                |serving| Blocker::ServiceServesAnotherHome {
                    home: home.to_path_buf(),
                    serving,
                    free_port: free_port_after(server.http_port),
                },
            )),
            None => RepairOutcome::Answering(graphql),
        };
        return Ok(repairs.report(outcome));
    }
    let Some(manager) = local_service(home, &current_executable()?)? else {
        return Ok(
            repairs.report(RepairOutcome::Blocked(Blocker::NoServiceManager {
                home: home.to_path_buf(),
            })),
        );
    };
    let config = manager.config().clone();
    repairs.step(RepairStep::StartNode);
    let sample = on_manager(&config, |manager| {
        if let ServiceHolder::Another { home, definition } = manager.holder()? {
            return Ok(Err(home
                .or(definition)
                .unwrap_or_else(|| manager.definition_path())));
        }
        match manager.installed_home()? {
            None => manager.install()?,
            Some(installed) => {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                let owned = manager.definition_owned_by_home()?;
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                let owned = installed == manager.home();
                if !owned {
                    return Ok(Err(installed));
                }
            }
        }
        let runs = manager.last_exit()?.map(|exit| exit.restarts);
        Ok(Ok((manager.status()?.running, runs)))
    })
    .await;
    let (running, runs) = match sample {
        Ok(Ok(sample)) => sample,
        Ok(Err(serving)) => {
            let port = ServerConfig::standard(home.to_path_buf()).http_port;
            return Ok(
                repairs.report(RepairOutcome::Blocked(Blocker::ServiceServesAnotherHome {
                    home: home.to_path_buf(),
                    serving,
                    free_port: free_port_after(port),
                })),
            );
        }
        Err(blocker) => return Ok(repairs.report(RepairOutcome::Blocked(blocker))),
    };
    let mut started = false;
    if !running {
        let server = ServerConfig::standard(home.to_path_buf());
        let port = server.http_port;
        match observe_port(&server, read_initialized_did(home).await.as_deref()).await {
            PortObservation::NotListening => {
                if let Err(blocker) = start_job(&config).await {
                    return Ok(repairs.report(RepairOutcome::Blocked(blocker)));
                }
                started = true;
            }
            PortObservation::Gents {
                identity: ServedIdentity::Matches,
                ..
            } => {}
            PortObservation::Gents {
                identity: ServedIdentity::Different { home, .. },
                ..
            } => {
                return Ok(
                    repairs.report(RepairOutcome::Blocked(Blocker::HandStartedNode {
                        port,
                        home,
                    })),
                )
            }
            PortObservation::NotGents => {
                return Ok(repairs.report(RepairOutcome::Blocked(Blocker::PortInUse {
                    port,
                    holder: port_holder(port).await,
                })))
            }
        }
    }
    repairs.step(RepairStep::WaitForNode);
    let since = Since {
        platform: NativeServicePlatform::current()?,
        runs,
        started,
    };
    let outcome = wait_for_node(home, &manager, since, serving_home).await?;
    Ok(repairs.report(outcome))
}

/// The other home the per-user service runs a node for, when it does: a
/// node on this home's port that is not this home's is then the service's.
async fn service_serving_another(home: &Path) -> Option<PathBuf> {
    let manager = local_service(home, &current_executable().ok()?).ok()??;
    on_manager(manager.config(), |manager| {
        Ok(match manager.holder()? {
            ServiceHolder::Another {
                home: Some(serving),
                ..
            } if manager.job_pid()?.is_some() => Some(serving),
            _ => None,
        })
    })
    .await
    .ok()
    .flatten()
}

/// The first loopback port after `taken` that nothing listens on now, for a
/// node started by hand beside the service's.
fn free_port_after(taken: u16) -> Option<u16> {
    (taken.checked_add(1)?..=taken.saturating_add(100))
        .find(|port| std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, *port)).is_ok())
}
fn answering_server(home: &Path, graphql: &str) -> Result<ServerConfig> {
    let url = url::Url::parse(graphql).context("parsing the recorded GraphQL endpoint")?;
    let http_addr = match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.into(),
        Some(url::Host::Ipv6(ip)) => ip.into(),
        _ => std::net::Ipv4Addr::LOCALHOST.into(),
    };
    Ok(ServerConfig {
        home: home.to_path_buf(),
        http_addr,
        http_port: url
            .port_or_known_default()
            .context("the recorded GraphQL endpoint names no port")?,
    })
}
fn hand_started(port: u16, observed: PortObservation) -> Option<Blocker> {
    match observed {
        PortObservation::Gents {
            identity:
                ServedIdentity::Different {
                    node_did: Some(_),
                    home,
                },
            ..
        } => Some(Blocker::HandStartedNode { port, home }),
        _ => None,
    }
}
async fn wait_for_node(
    home: &Path,
    manager: &NativeServiceManager,
    since: Since,
    serving_home: Option<&Path>,
) -> Result<RepairOutcome> {
    let config = manager.config();
    let deadline = Instant::now() + ENDPOINT_WAIT;
    let mut next_sample = Instant::now() + SAMPLE_EVERY;
    let mut watch = CrashLoopWatch::default();
    loop {
        if let Some(graphql) = answering_graphql(home).await? {
            return Ok(RepairOutcome::Answering(graphql));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(RepairOutcome::Blocked(Blocker::NoAnswer {
                seconds: ENDPOINT_WAIT.as_secs(),
                last_error: last_error(config).await,
            }));
        }
        if now >= next_sample {
            next_sample = now + SAMPLE_EVERY;
            let sample = on_manager(config, |manager| {
                Ok((manager.status()?.job_loaded, manager.last_exit()?))
            })
            .await;
            let (job_loaded, last_exit) = match sample {
                Ok(sample) => sample,
                Err(blocker) => return Ok(RepairOutcome::Blocked(blocker)),
            };
            match last_exit.as_ref().and_then(|exit| {
                let action = on_refusal(exit, since.platform, since.runs, since.started)?;
                Some((exit, action))
            }) {
                Some((exit, OnRefusal::Block)) => {
                    return Ok(RepairOutcome::Blocked(blocker_in(
                        home,
                        exit,
                        last_error(config).await,
                    )));
                }
                Some((_, OnRefusal::Ask)) => {
                    return Ok(RepairOutcome::Decide(
                        decision(home, HomeState::MissingStoreKey, serving_home).await?,
                    ));
                }
                None => {}
            }
            if let Some((exit, _)) =
                watch.observe(job_sample(false, job_loaded, last_exit.as_ref()))
            {
                return Ok(RepairOutcome::Blocked(blocker_in(
                    home,
                    &exit,
                    last_error(config).await,
                )));
            }
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnRefusal {
    Ask,
    Block,
}
#[derive(Debug, Clone, Copy)]
struct Since {
    platform: NativeServicePlatform,
    runs: Option<u64>,
    started: bool,
}
fn on_refusal(
    exit: &ServiceExit,
    platform: NativeServicePlatform,
    runs: Option<u64>,
    started: bool,
) -> Option<OnRefusal> {
    let fresh = match platform {
        NativeServicePlatform::Linux if started => true,
        _ => runs.is_none_or(|runs| exit.restarts > runs),
    };
    if !fresh {
        return None;
    }
    use gents_protocol::operations::install_repair::{RefusalAction, RefusalObservation};
    let refusal = exit.refusal()?;
    let kind = match refusal {
        ServiceRefusal::Store(kind) => Some(kind),
        _ => None,
    };
    let action = planning::refusal_action(&RefusalObservation {
        fresh,
        missing_key: kind == Some(IncompatibleStoreKind::MissingStoreKey),
        older_store: kind.is_some_and(IncompatibleStoreKind::is_older),
        newer_store: kind == Some(IncompatibleStoreKind::ForeignVersion),
    });
    match action {
        RefusalAction::Ignore => None,
        RefusalAction::Ask => Some(OnRefusal::Ask),
        RefusalAction::Block => Some(OnRefusal::Block),
    }
}
/// The store a node's last exit refused as an older version's, read from the
/// store's own markers first since one exit code covers several kinds.
async fn older_refused(home: &Path, last_exit: Option<&ServiceExit>) -> Option<IncompatibleStore> {
    let Some(ServiceRefusal::Store(reported)) = last_exit?.refusal() else {
        return None;
    };
    let data = default_data_dir(home);
    let probe = data.clone();
    let on_disk = tokio::task::spawn_blocking(move || incompatible_store_kind(&probe))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    let kind = on_disk.unwrap_or(reported);
    kind.made_by_older_version().then_some(IncompatibleStore {
        kind,
        data_path: data,
    })
}
fn blocker_in(home: &Path, exit: &ServiceExit, last_error: Option<String>) -> Blocker {
    let logged = last_error.is_some();
    match blocker_for_exit(exit, last_error) {
        Blocker::StoreRefused { kind, .. } if !logged => {
            let data_path = match kind {
                IncompatibleStoreKind::InsecureKey => home.join("keys"),
                _ => default_data_dir(home),
            };
            Blocker::StoreRefused {
                kind,
                message: IncompatibleStore { kind, data_path }.to_string(),
            }
        }
        blocker => blocker,
    }
}
async fn last_error(config: &NativeServiceConfig) -> Option<String> {
    let legacy = config
        .user_home
        .join("Library/Application Support/gents/desktop/logs/runtime-errors.log");
    read_last_error(&config.user_home, Some(&legacy)).await
}

/// Whether the runtime `home` records answers, and where. Only that it
/// answers matters here, so no client identity is loaded.
pub(crate) async fn answering_graphql(home: &Path) -> Result<Option<String>> {
    match read_runtime_state(home)? {
        Some(state)
            if graphql_endpoint_available(
                &state.graphql,
                GraphqlRequestOptions {
                    timeout: Duration::from_secs(2),
                    max_attempts: 1,
                    retry_backoff: Duration::from_millis(50),
                },
            )
            .await =>
        {
            Ok(Some(state.graphql))
        }
        _ => Ok(None),
    }
}
fn local_service(home: &Path, executable: &Path) -> Result<Option<NativeServiceManager>> {
    if NativeServicePlatform::current().is_err() {
        return Ok(None);
    }
    let config = NativeServiceConfig::new(home.to_path_buf(), executable.to_path_buf())?;
    Ok(Some(NativeServiceManager::new(config)?))
}
pub fn current_executable() -> Result<PathBuf> {
    std::env::current_exe().context("resolving the gents executable")
}
pub fn current_program() -> Result<PathBuf, Blocker> {
    current_executable()
        .and_then(|path| std::fs::canonicalize(&path).context("resolving the gents executable"))
        .map_err(|error| Blocker::ServiceUpdateFailed {
            reason: format!("{error:#}"),
        })
}

fn repair_lock_path() -> Result<PathBuf> {
    Ok(gents_store::local::home::gents_config_dir()?.join("repair.lock"))
}
async fn hold_lock(path: &Path) -> Result<FileLock> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<FileLock> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        FileLock::exclusive(file).with_context(|| format!("locking {}", path.display()))
    })
    .await?
}

fn blocked(blocker: Blocker) -> RepairReceipt {
    RepairReceipt {
        client: None,
        repaired: Vec::new(),
        outcome: RepairOutcome::Blocked(blocker),
        serving_home: None,
        older_data: None,
        unfinished_setup: false,
    }
}

struct Repairs<'a> {
    progress: &'a mut dyn Progress,
    repaired: Vec<Finding>,
    done: usize,
    total: usize,
}

impl<'a> Repairs<'a> {
    fn new(progress: &'a mut dyn Progress, total: usize) -> Self {
        Self {
            progress,
            repaired: Vec::new(),
            done: 0,
            total,
        }
    }

    fn found(&mut self, finding: Finding) {
        self.progress.finding(&finding);
        self.repaired.push(finding);
    }

    fn step(&mut self, step: RepairStep) {
        self.done += 1;
        self.progress.step(self.done, self.total, step);
    }
    fn extra_step(&mut self, step: RepairStep) {
        self.total += 1;
        self.step(step);
    }

    fn report(self, outcome: RepairOutcome) -> RepairReceipt {
        RepairReceipt {
            client: None,
            repaired: self.repaired,
            outcome,
            serving_home: None,
            older_data: None,
            unfinished_setup: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepairFailure {
    #[error("installation repair failed: {source}")]
    Operation {
        #[source]
        source: anyhow::Error,
    },
}

#[async_trait::async_trait]
pub trait ClientExclusion: Send + Sync {
    async fn exclude(&self)
        -> std::result::Result<tokio::sync::OwnedMutexGuard<()>, RepairFailure>;
}

impl From<anyhow::Error> for RepairFailure {
    fn from(source: anyhow::Error) -> Self {
        Self::Operation { source }
    }
}

pub async fn execute(
    input: gents_protocol::operations::install_repair::RepairInput,
    program: &ServiceProgram,
    progress: &mut dyn Progress,
    client_lifecycle: Option<&dyn ClientExclusion>,
) -> std::result::Result<RepairReceipt, Blocker> {
    use gents_protocol::operations::install_repair::RepairInput;
    let result = match input {
        RepairInput::Prepare {
            choice,
            service_move,
            operation,
            interactive,
            answer,
        } => {
            prepare(
                &choice,
                program,
                service_move,
                operation,
                Asked {
                    interactive,
                    answer,
                },
                progress,
            )
            .await
        }
        RepairInput::Start {
            home,
            serving_home,
            older_data: _,
        } => start(&home, serving_home.as_deref(), progress).await,
        RepairInput::Restore { choice } => restore(&choice, progress).await,
        RepairInput::Client { input } => {
            let lifecycle = client_lifecycle.ok_or_else(|| Blocker::ServiceUpdateFailed {
                reason: "client repair requires its lifecycle owner".to_owned(),
            })?;
            let mut client = Default::default();
            let outcome = match client::execute(&input, lifecycle, &mut client).await {
                Ok(()) => RepairOutcome::Ready,
                Err(blocker) => RepairOutcome::Blocked(blocker),
            };
            return Ok(RepairReceipt {
                client: Some(client),
                repaired: Vec::new(),
                outcome,
                serving_home: None,
                older_data: None,
                unfinished_setup: false,
            });
        }
    };
    Ok(match result {
        Ok(receipt) => receipt,
        Err(error) => blocked(Blocker::ServiceUpdateFailed {
            reason: gents_protocol::error_chain::ErrorChain(error.as_ref()).to_string(),
        }),
    })
}

#[cfg(test)]
mod conformance;
#[cfg(all(test, unix))]
mod filesystem_tests;
