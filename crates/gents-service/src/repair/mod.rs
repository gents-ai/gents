use gents_protocol::operations::install_repair::{
    Blocker, Finding, HomeChoice, HomeDecision, HomeState, OlderData, RepairOperation,
    RepairOutcome, RepairReceipt, RepairStep, ServiceMove,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gents_protocol::operations::install_repair::{IncompatibleStore, IncompatibleStoreKind};
use gents_store::config_client::GraphqlEndpoint;
use gents_store::local::file_lock::FileLock;
use gents_store::local::home::default_data_dir;
use gents_store::local::storage_backend::incompatible_store_kind;
use tokio::time::Instant;

use crate::home_state::{default_home_ignoring_env, read_runtime_state};
use crate::native_service::{
    NativeServiceConfig, NativeServiceManager, NativeServicePlatform, ServiceExit, ServiceRefusal,
};
use crate::repair::crash_loop::{job_sample, CrashLoopWatch};
use crate::repair::home::{
    aside_label, classify_home, held_by_another, older_label, set_aside_home_as,
    unless_being_set_up,
};
use crate::repair::port::{
    observe_port, port_holder, read_initialized_did, served_identity, PortObservation,
    ServedIdentity,
};
use crate::repair::reason::{blocker_for_exit, read_last_error};
use gents_protocol::graphql::{graphql_endpoint_available, GraphqlRequestOptions};

use crate::repair::service::{
    apply, gather_facts, newer_node, on_manager, plan_service, served_version, start_job,
    ServiceAction, ServiceFacts,
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
async fn prepare(
    choice: &HomeChoice,
    program: &ServiceProgram,
    service_move: ServiceMove,
    operation: RepairOperation,
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
    let chosen = choice.home.clone();
    let home =
        match tokio::task::spawn_blocking(move || classify_home(&chosen, last_refusal)).await? {
            Ok(home) => home,
            Err(blocker) => return Ok(blocked(blocker)),
        };
    let home =
        if home == HomeState::Healthy && older_refused(choice, last_exit.as_ref(), running).await {
            HomeState::OlderStore
        } else {
            home
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
        },
    };
    facts.node_version = served_version(&facts.port);
    facts.was_serving = was_serving;
    facts.service_move = service_move;
    facts.operation = operation;
    facts.served_home_present =
        served_home_present(facts.definition_home.as_deref(), &choice.home).await;
    let busy = home_busy(manager.as_ref(), &facts, &choice.home).await;
    let serving_home =
        serving_home(facts.definition_home.as_deref(), &facts.port, &choice.home).await;
    let mut report = settle(
        choice,
        &facts,
        busy,
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
        !matches!(classify_home(&served, None), Ok(HomeState::Absent))
    })
    .await
    .unwrap_or(true)
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

async fn settle(
    choice: &HomeChoice,
    facts: &ServiceFacts,
    busy: bool,
    manager: Option<&NativeServiceManager>,
    serving_home: Option<&Path>,
    program: &ServiceProgram,
    progress: &mut dyn Progress,
) -> Result<RepairReceipt> {
    use gents_protocol::operations::install_repair::{
        RepairObservation, RepairPlanBlocker, RepairVersion,
    };
    let observation = RepairObservation {
        home: facts.home,
        version: match &facts.node_version {
            Some(service::NodeVersion::Older(_)) => RepairVersion::Older,
            Some(service::NodeVersion::Same) => RepairVersion::Same,
            Some(service::NodeVersion::Newer(_)) => RepairVersion::Newer,
            None => RepairVersion::Unknown,
        },
        explicit_home: choice.explicit,
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
    };
    let plan = planning::plan(&observation);
    match plan.blocker {
        RepairPlanBlocker::None => {}
        RepairPlanBlocker::ExplicitHome => {
            return Ok(RepairReceipt {
                client: None,
                repaired: Vec::new(),
                outcome: RepairOutcome::Decide(
                    decision(&choice.home, facts.home, serving_home).await?,
                ),
                serving_home: serving_home.map(Path::to_path_buf),
                older_data: None,
            });
        }
        RepairPlanBlocker::NewerVersion => {
            return Ok(blocked(newer_node(facts).unwrap_or_else(|| {
                Blocker::StoreRefused {
                    kind: IncompatibleStoreKind::ForeignVersion,
                    message: IncompatibleStore {
                        kind: IncompatibleStoreKind::ForeignVersion,
                        data_path: default_data_dir(&choice.home),
                    }
                    .to_string(),
                }
            })))
        }
        _ => {
            return Ok(blocked(Blocker::HomeBusy {
                home: choice.home.clone(),
            }))
        }
    }
    let actions = plan_service(facts, &choice.home);
    let answered = answered_switch(&actions);
    let answered_from = answered.as_ref().and_then(|(from, _)| from.clone());
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
    if home::broken(facts.home).is_some() {
        steps.push(RepairStep::SetAsideFolder);
    }
    let mut repairs = Repairs::new(progress, steps.len());
    for finding in actions.iter().filter_map(finding_for) {
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
    let mut outcome = match (&facts.home, started_from, manager) {
        (HomeState::Healthy, Some(from), Some(manager)) => {
            repairs.step(RepairStep::WaitForNode);
            opens_after_switch(&choice.home, manager, from, &mut repairs)
                .await
                .unwrap_or_else(repair_error)
        }
        (HomeState::Healthy, None, Some(manager)) if replacing == Some(true) => {
            repairs.step(RepairStep::WaitForNode);
            let serving = serving_home.filter(|_| choice.explicit);
            opens_after_replacing(
                &choice.home,
                manager,
                serving,
                replaced.as_ref(),
                &mut repairs,
            )
            .await
            .unwrap_or_else(repair_error)
        }
        (HomeState::Absent, ..) => RepairOutcome::NeedsSetup,
        (HomeState::Healthy, ..) => RepairOutcome::Ready,
        (why, ..) => {
            repairs.step(RepairStep::SetAsideFolder);
            let (older, label) = match why {
                HomeState::OlderStore => {
                    let from = answered_from
                        .clone()
                        .or_else(|| replaced.as_ref().and_then(|replaced| replaced.from.clone()));
                    let label = older_label(from.as_ref());
                    (Some(from), label)
                }
                _ => {
                    let older = answered.map(|(from, _)| from);
                    let label = aside_label(older.as_ref().and_then(Option::as_ref));
                    (older, label)
                }
            };
            match set_aside_home_as(&choice.home, label).await {
                Ok(kept_at) => {
                    repairs.found(broken_set_aside(*why, older, kept_at));
                    RepairOutcome::NeedsSetup
                }
                Err(blocker) => RepairOutcome::Blocked(blocker),
            }
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
            from: answered_from,
        })
    });
    Ok(report)
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
    older_data: Option<&OlderData>,
    repairs: &mut Repairs<'_>,
) -> Result<RepairOutcome> {
    let since = Since {
        platform: NativeServicePlatform::current()?,
        runs: None,
        started: true,
    };
    let kept = |kept_at| Finding::BrokenHome {
        why: HomeState::MissingStoreKey,
        kept_at,
    };
    let waited = wait_for_node(
        home,
        manager,
        since,
        serving_home,
        &aside_label(None),
        &kept,
        older_data,
        repairs,
    )
    .await?;
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
fn broken_set_aside(
    why: HomeState,
    older: Option<Option<semver::Version>>,
    kept_at: PathBuf,
) -> Finding {
    match older {
        Some(from) => Finding::OlderDataSetAside { from, kept_at },
        None => Finding::BrokenHome { why, kept_at },
    }
}
async fn opens_after_switch(
    home: &Path,
    manager: &NativeServiceManager,
    from: Option<semver::Version>,
    repairs: &mut Repairs<'_>,
) -> Result<RepairOutcome> {
    let since = Since {
        platform: NativeServicePlatform::current()?,
        runs: None,
        started: true,
    };
    let label = aside_label(from.as_ref());
    let kept = |kept_at| Finding::OlderDataSetAside {
        from: from.clone(),
        kept_at,
    };
    let waited = wait_for_node(home, manager, since, None, &label, &kept, None, repairs).await?;
    Ok(match waited {
        RepairOutcome::Answering(_) => RepairOutcome::Ready,
        RepairOutcome::Ready => RepairOutcome::Ready,
        RepairOutcome::NeedsSetup => RepairOutcome::NeedsSetup,
        RepairOutcome::Decide(decision) => RepairOutcome::Decide(decision),
        RepairOutcome::Blocked(blocker) => RepairOutcome::Blocked(blocker),
    })
}
async fn stop_and_set_aside(
    home: &Path,
    manager: &NativeServiceManager,
    label: &str,
    repairs: &mut Repairs<'_>,
) -> Result<PathBuf, Blocker> {
    apply(&ServiceAction::StopStale, manager, false, false).await?;
    repairs.extra_step(RepairStep::SetAsideFolder);
    set_aside_home_as(home, label.to_string()).await
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
        | ServiceAction::RefreshLog { .. } => RepairStep::UpdateService,
    }
}

fn finding_for(action: &ServiceAction) -> Option<Finding> {
    match action {
        ServiceAction::MoveFrom { from, .. } => {
            Some(Finding::ServiceForAnotherFolder { from: from.clone() })
        }
        ServiceAction::StopStale => Some(Finding::StaleNode),
        ServiceAction::Reinstall { .. } => Some(Finding::ServiceProgramMissing),
        ServiceAction::SwitchProgram { .. }
        | ServiceAction::RefreshLog { .. }
        | ServiceAction::Restart => None,
    }
}
async fn start(
    home: &Path,
    serving_home: Option<&Path>,
    older_data: Option<&OlderData>,
    progress: &mut dyn Progress,
) -> Result<RepairReceipt> {
    let _lock = hold_lock(&repair_lock_path()?).await?;
    let mut repairs = Repairs::new(progress, 2);
    if let Some(graphql) = answering_graphql(home).await? {
        let server = answering_server(home, &graphql)?;
        let observed = observe_port(&server, read_initialized_did(home).await.as_deref()).await;
        return Ok(
            repairs.report(match hand_started(server.http_port, observed) {
                Some(blocker) => RepairOutcome::Blocked(blocker),
                None => RepairOutcome::Answering(graphql.url().to_owned()),
            }),
        );
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
            return Ok(
                repairs.report(RepairOutcome::Blocked(Blocker::ServiceServesAnotherHome {
                    home: home.to_path_buf(),
                    serving,
                })),
            )
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
    let kept = |kept_at| Finding::BrokenHome {
        why: HomeState::MissingStoreKey,
        kept_at,
    };
    let outcome = wait_for_node(
        home,
        &manager,
        since,
        serving_home,
        &aside_label(None),
        &kept,
        older_data,
        &mut repairs,
    )
    .await?;
    Ok(repairs.report(outcome))
}
fn answering_server(home: &Path, graphql: &GraphqlEndpoint) -> Result<ServerConfig> {
    let url = url::Url::parse(graphql.url()).context("parsing the recorded GraphQL endpoint")?;
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
#[expect(
    clippy::too_many_arguments,
    reason = "each caller names its own set-aside, and the manager is injected"
)]
async fn wait_for_node(
    home: &Path,
    manager: &NativeServiceManager,
    since: Since,
    serving_home: Option<&Path>,
    label: &str,
    kept: &(dyn Fn(PathBuf) -> Finding + Sync),
    older_data: Option<&OlderData>,
    repairs: &mut Repairs<'_>,
) -> Result<RepairOutcome> {
    let config = manager.config();
    let deadline = Instant::now() + ENDPOINT_WAIT;
    let mut next_sample = Instant::now() + SAMPLE_EVERY;
    let mut watch = CrashLoopWatch::default();
    loop {
        if let Some(graphql) = answering_graphql(home).await? {
            return Ok(RepairOutcome::Answering(graphql.url().to_owned()));
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
                let action = on_refusal(
                    exit,
                    since.platform,
                    since.runs,
                    since.started,
                    serving_home,
                )?;
                Some((exit, action))
            }) {
                Some((exit, OnRefusal::Block)) => {
                    let older = match older_data {
                        Some(older_data) => {
                            refused_older_data(home, exit).await.map(|_| older_data)
                        }
                        None => None,
                    };
                    if let Some(older_data) = older {
                        let from = older_data.from.clone();
                        let label = older_label(from.as_ref());
                        return Ok(
                            match stop_and_set_aside(home, manager, &label, repairs).await {
                                Ok(kept_at) => {
                                    repairs.found(Finding::OlderDataSetAside { from, kept_at });
                                    RepairOutcome::NeedsSetup
                                }
                                Err(blocker) => RepairOutcome::Blocked(blocker),
                            },
                        );
                    }
                    return Ok(RepairOutcome::Blocked(blocker_in(
                        home,
                        exit,
                        last_error(config).await,
                    )));
                }
                Some((_, OnRefusal::Ask(serving))) => {
                    return Ok(RepairOutcome::Decide(
                        decision(home, HomeState::MissingStoreKey, Some(serving)).await?,
                    ));
                }
                Some((_, OnRefusal::SetAside)) => {
                    return Ok(
                        match stop_and_set_aside(home, manager, label, repairs).await {
                            Ok(kept_at) => {
                                repairs.found(kept(kept_at));
                                RepairOutcome::NeedsSetup
                            }
                            Err(blocker) => RepairOutcome::Blocked(blocker),
                        },
                    );
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
enum OnRefusal<'a> {
    Ask(&'a Path),
    SetAside,
    Block,
}
#[derive(Debug, Clone, Copy)]
struct Since {
    platform: NativeServicePlatform,
    runs: Option<u64>,
    started: bool,
}
fn on_refusal<'a>(
    exit: &ServiceExit,
    platform: NativeServicePlatform,
    runs: Option<u64>,
    started: bool,
    serving_home: Option<&'a Path>,
) -> Option<OnRefusal<'a>> {
    let fresh = match platform {
        NativeServicePlatform::Linux if started => true,
        _ => runs.is_none_or(|runs| exit.restarts > runs),
    };
    if !fresh {
        return None;
    }
    use gents_protocol::operations::install_repair::{RefusalAction, RefusalObservation};
    let refusal = exit.refusal()?;
    let action = planning::refusal_action(&RefusalObservation {
        fresh,
        missing_key: refusal == ServiceRefusal::Store(IncompatibleStoreKind::MissingStoreKey),
        explicit_home: serving_home.is_some(),
        older_store: false,
        newer_store: refusal == ServiceRefusal::Store(IncompatibleStoreKind::ForeignVersion),
    });
    match action {
        RefusalAction::Ignore => None,
        RefusalAction::Ask => serving_home.map(OnRefusal::Ask),
        RefusalAction::Archive => Some(OnRefusal::SetAside),
        RefusalAction::Block => Some(OnRefusal::Block),
    }
}
async fn refused_older_data(home: &Path, exit: &ServiceExit) -> Option<IncompatibleStoreKind> {
    let Some(ServiceRefusal::Store(reported)) = exit.refusal() else {
        return None;
    };
    let data = default_data_dir(home);
    let on_disk = tokio::task::spawn_blocking(move || incompatible_store_kind(&data))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    Some(on_disk.unwrap_or(reported)).filter(|kind| cannot_open(*kind))
}
fn cannot_open(kind: IncompatibleStoreKind) -> bool {
    !matches!(
        kind,
        IncompatibleStoreKind::ForeignVersion | IncompatibleStoreKind::MissingStoreKey
    )
}
pub fn sets_aside_refused(choice: &HomeChoice, kind: IncompatibleStoreKind) -> bool {
    use gents_protocol::operations::install_repair::{RefusalAction, RefusalObservation};
    planning::refusal_action(&RefusalObservation {
        fresh: true,
        missing_key: false,
        explicit_home: choice.explicit,
        older_store: cannot_open(kind),
        newer_store: kind == IncompatibleStoreKind::ForeignVersion,
    }) == RefusalAction::Archive
}
async fn set_aside_refused(
    choice: &HomeChoice,
    progress: &mut dyn Progress,
) -> Result<RepairReceipt> {
    let _lock = hold_lock(&repair_lock_path()?).await?;
    let mut repairs = Repairs::new(progress, 1);
    repairs.step(RepairStep::StopOldNode);
    let Some(manager) = local_service(&choice.home, &current_executable()?)? else {
        return Ok(
            repairs.report(RepairOutcome::Blocked(Blocker::NoServiceManager {
                home: choice.home.clone(),
            })),
        );
    };
    let outcome =
        match stop_and_set_aside(&choice.home, &manager, &older_label(None), &mut repairs).await {
            Ok(kept_at) => {
                repairs.found(Finding::OlderDataSetAside {
                    from: None,
                    kept_at,
                });
                RepairOutcome::NeedsSetup
            }
            Err(blocker) => RepairOutcome::Blocked(blocker),
        };
    Ok(repairs.report(outcome))
}
async fn older_refused(
    choice: &HomeChoice,
    last_exit: Option<&ServiceExit>,
    running: bool,
) -> bool {
    match last_exit {
        Some(exit) if !choice.explicit && !running => {
            refused_older_data(&choice.home, exit).await.is_some()
        }
        _ => false,
    }
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

pub(crate) async fn answering_graphql(home: &Path) -> Result<Option<GraphqlEndpoint>> {
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
            Ok(Some(crate::home_state::resolve_graphql_endpoint(
                Some(&state.graphql),
                Some(home),
            )?))
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
        } => prepare(&choice, program, service_move, operation, progress).await,
        RepairInput::Start {
            home,
            serving_home,
            older_data,
        } => {
            start(
                &home,
                serving_home.as_deref(),
                older_data.as_ref(),
                progress,
            )
            .await
        }
        RepairInput::Restore { choice } => restore(&choice, progress).await,
        RepairInput::Refused { choice } => set_aside_refused(&choice, progress).await,
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
