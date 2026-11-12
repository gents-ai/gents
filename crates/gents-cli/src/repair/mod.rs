use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gents::store::config_client::GraphqlEndpoint;
use gents::store::local::file_lock::FileLock;
use gents::store::local::home::default_data_dir;
use gents::store::local::storage_backend::{
    incompatible_store_kind, IncompatibleStore, IncompatibleStoreKind,
};
use tokio::time::Instant;

use crate::graphql_access::graphql_endpoint_available;
use crate::home_state::{default_home_ignoring_env, read_runtime_state};
use crate::native_service::{
    NativeServiceConfig, NativeServiceManager, NativeServicePlatform, ServiceExit, ServiceRefusal,
    BACKGROUND_APPROVAL_REQUIRED,
};
use crate::repair::crash_loop::{job_sample, CrashLoopWatch};
use crate::repair::home::{
    aside_label, classify_home, older_label, set_aside_home_as, unless_being_set_up, BrokenHome,
    HomeState,
};
use crate::repair::port::{
    observe_port, port_holder, read_initialized_did, served_identity, PortHolder, PortObservation,
    ServedIdentity,
};
use crate::repair::reason::{blocker_for_exit, read_last_error};
pub use crate::repair::service::ServiceMove;
use crate::repair::service::{
    apply, gather_facts, newer_node, on_manager, plan_service, served_version, start_job,
    ServiceAction, ServiceFacts,
};
use crate::server_host::ServerConfig;

pub mod crash_loop;
pub mod home;
pub mod port;
pub mod reason;
pub mod service;

/// Why the local node cannot start, in words the user can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Blocker {
    PortInUse {
        port: u16,
        holder: Option<PortHolder>,
    },
    HandStartedNode {
        port: u16,
        home: Option<String>,
    },
    KeychainUnavailable,
    StoreRefused {
        kind: IncompatibleStoreKind,
        message: String,
    },
    CannotSetAside {
        home: PathBuf,
        reason: String,
    },
    KeepsExiting {
        code: Option<i32>,
        last_error: Option<String>,
    },
    NoAnswer {
        seconds: u64,
        last_error: Option<String>,
    },
    NoServiceManager {
        home: PathBuf,
    },
    HomeUnreadable {
        home: PathBuf,
        reason: String,
    },
    HomeBusy {
        home: PathBuf,
    },
    OldNodeStillRunning {
        pid: u32,
    },
    ServiceUpdateFailed {
        reason: String,
    },
    BackgroundApprovalPending {
        detail: Option<String>,
    },
    NewerNode {
        running: String,
        ours: String,
    },
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PortInUse {
                port,
                holder: Some(holder),
            } => write!(
                f,
                "port {port} is in use by {} (pid {}). Quit it, then try again.",
                holder.program, holder.pid
            ),
            Self::PortInUse { port, holder: None } => write!(
                f,
                "port {port} is in use by another app. Quit it, then try again."
            ),
            Self::HandStartedNode { port, home } => {
                write!(f, "port {port} is used by a Gents node started by hand")?;
                if let Some(home) = home {
                    write!(f, " from {home}")?;
                }
                f.write_str(". Stop it, then try again.")
            }
            Self::KeychainUnavailable => f.write_str(
                "the macOS Keychain is locked or Gents isn't allowed to read it. Unlock it, then try again.",
            ),
            Self::StoreRefused { message, .. } => f.write_str(message),
            Self::CannotSetAside { home, reason } => {
                write!(f, "{} couldn't be set aside: {reason}", home.display())
            }
            Self::KeepsExiting { code, last_error } => {
                f.write_str("the background node keeps exiting")?;
                match (last_error, code) {
                    (Some(error), _) => write!(f, ": {error}"),
                    (None, Some(code)) => write!(f, ": exit code {code}"),
                    (None, None) => Ok(()),
                }
            }
            Self::NoAnswer {
                seconds,
                last_error,
            } => {
                write!(f, "the background node did not answer within {seconds} s")?;
                match last_error {
                    Some(error) => write!(f, ": {error}"),
                    None => Ok(()),
                }
            }
            Self::NoServiceManager { home } => {
                f.write_str(&reason::no_service_manager_message(home))
            }
            Self::HomeUnreadable { home, reason } => write!(
                f,
                "{} can't be read: {reason}. Check its permissions, then try again.",
                home.display()
            ),
            Self::HomeBusy { home } => write!(
                f,
                "{} is being set up right now. Try again when it finishes.",
                home.display()
            ),
            Self::OldNodeStillRunning { pid } => write!(
                f,
                "the old background node (pid {pid}) did not stop. Quit it, then try again."
            ),
            Self::ServiceUpdateFailed { reason } => {
                write!(f, "the background service could not be updated: {reason}")
            }
            Self::BackgroundApprovalPending { .. } => f.write_str(BACKGROUND_APPROVAL_REQUIRED),
            Self::NewerNode { running, ours } => write!(
                f,
                "Your local node runs Gents {running}, newer than this Gents ({ours}). Open Gents {running} or newer, then try again."
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeChoice {
    pub home: PathBuf,
    pub explicit: bool,
}

/// Something the routine found wrong and repaired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    StaleNode,
    ServiceForAnotherFolder {
        from: PathBuf,
    },
    ServiceProgramMissing,
    BrokenHome {
        why: BrokenHome,
        kept_at: PathBuf,
    },
    MovedToThisVersion {
        from: Option<semver::Version>,
    },
    OlderDataSetAside {
        from: Option<semver::Version>,
        kept_at: PathBuf,
    },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleNode => {
                f.write_str("the background node was still serving a deleted or replaced folder")
            }
            Self::ServiceForAnotherFolder { from } => {
                write!(f, "the background node was serving {}", from.display())
            }
            Self::ServiceProgramMissing => {
                f.write_str("the background service pointed at a copy of Gents that is gone")
            }
            Self::BrokenHome { why, kept_at } => {
                write!(f, "{why}; it was set aside as {}", kept_at.display())
            }
            Self::MovedToThisVersion { from: Some(from) } => write!(
                f,
                "Gents moved your local node from version {from} to this version"
            ),
            Self::MovedToThisVersion { from: None } => {
                f.write_str("Gents moved your local node to this version")
            }
            Self::OlderDataSetAside { from, kept_at } => {
                f.write_str("this version couldn't open the data from ")?;
                match from {
                    Some(from) => write!(f, "Gents {from}")?,
                    None => f.write_str("an older Gents version")?,
                }
                write!(f, ", so it was set aside at {}", kept_at.display())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairStep {
    StopOldNode,
    UpdateService,
    SetAsideFolder,
    StartNode,
    WaitForNode,
}

impl fmt::Display for RepairStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StopOldNode => "Stopping the old node",
            Self::UpdateService => "Updating the background service",
            Self::SetAsideFolder => "Setting aside the old folder",
            Self::StartNode => "Starting the node",
            Self::WaitForNode => "Waiting for the node",
        })
    }
}

/// Observes the routine: a finding before its repair, then each step as it begins.
pub trait Progress: Send {
    fn finding(&mut self, finding: &Finding);
    fn step(&mut self, index: usize, total: usize, step: RepairStep);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeDecision {
    pub home: PathBuf,
    pub why: BrokenHome,
    pub default_home: PathBuf,
    /// The set-up folder the node was serving before this run moved it.
    pub serving_home: PathBuf,
}

#[derive(Debug)]
pub enum Prepared {
    Ready,
    NeedsSetup,
    Decide(HomeDecision),
    Blocked(Blocker),
}

#[derive(Debug)]
pub enum Started {
    Answering(GraphqlEndpoint),
    NeedsSetup,
    Decide(HomeDecision),
    Blocked(Blocker),
}

#[derive(Debug)]
pub struct Report<T> {
    pub repaired: Vec<Finding>,
    pub outcome: T,
    /// Set by `prepare` past onboarding: the other set-up folder the node
    /// serves, answering with its own id. A passed folder that fails is then
    /// asked about, and this folder gets its node back first.
    pub serving_home: Option<PathBuf>,
    /// Set by `prepare` when a start that follows sets aside older data the
    /// node refuses: after this run replaced the service's program or folder,
    /// or for the default home, which no one chose on purpose.
    pub older_data: Option<OlderData>,
}

/// Names the older data a refused home is set aside as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OlderData {
    /// The version of the program that ran before, when known: the bundle of
    /// a program this run replaced, or what its node answered.
    pub from: Option<semver::Version>,
}

pub(crate) const ENDPOINT_WAIT: Duration = Duration::from_secs(60);
const SAMPLE_EVERY: Duration = Duration::from_secs(1);
const POLL_EVERY: Duration = Duration::from_millis(100);

/// The program a reinstalled service runs. It is resolved only when a repair
/// writes it into a definition, since a caller may be unable to name one
/// (a desktop opened from a disk image) while its node is healthy.
pub type ServiceProgram = Arc<dyn Fn() -> Result<PathBuf, Blocker> + Send + Sync>;

/// Resolves `program` off the runtime: a desktop may copy its runtime first.
async fn resolve(program: &ServiceProgram) -> Result<PathBuf, Blocker> {
    let program = Arc::clone(program);
    tokio::task::spawn_blocking(move || program())
        .await
        .unwrap_or_else(|join| {
            Err(Blocker::ServiceUpdateFailed {
                reason: join.to_string(),
            })
        })
}

/// Brings the service and `choice.home` in line before setup. Installs or
/// starts nothing, except restarting a node a repair had to stop.
pub async fn prepare(
    choice: &HomeChoice,
    program: &ServiceProgram,
    service_move: ServiceMove,
    progress: &mut dyn Progress,
) -> Result<Report<Prepared>> {
    prepare_with_lock(
        &repair_lock_path()?,
        local_service(&choice.home, &current_executable()?)?,
        &ServerConfig::standard(choice.home.clone()),
        choice,
        false,
        program,
        service_move,
        progress,
    )
    .await
}

/// `prepare` for the folder that answered before this run moved its node
/// away: the service moves back and its node starts again.
pub async fn restore(choice: &HomeChoice, progress: &mut dyn Progress) -> Result<Report<Prepared>> {
    prepare_with_lock(
        &repair_lock_path()?,
        local_service(&choice.home, &current_executable()?)?,
        &ServerConfig::standard(choice.home.clone()),
        choice,
        true,
        &(Arc::new(current_program) as ServiceProgram),
        ServiceMove::AfterSetup,
        progress,
    )
    .await
}

/// `server` is where the chosen home's node answers.
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
    progress: &mut dyn Progress,
) -> Result<Report<Prepared>> {
    let _lock = hold_lock(lock).await?;
    let (last_exit, running) = match &manager {
        Some(manager) => {
            let sampled = on_manager(manager.config(), |manager| {
                // A refusal by another folder's node says nothing about this one.
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
    let home = if home == HomeState::Complete
        && older_refused(choice, last_exit.as_ref(), running).await
    {
        HomeState::Broken(BrokenHome::OlderVersionData)
    } else {
        home
    };
    let home = if home == HomeState::Broken(BrokenHome::SetupStoppedMidway) {
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
        Some(manager) => gather_facts(manager, &choice.home, server, home.clone(), program).await?,
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
            previous_version: None,
        },
    };
    facts.node_version = served_version(&facts.port);
    facts.was_serving = was_serving;
    facts.service_move = service_move;
    let serving_home =
        serving_home(facts.definition_home.as_deref(), &facts.port, &choice.home).await;
    let mut report = settle(
        choice,
        &facts,
        manager.as_ref(),
        serving_home.as_deref(),
        program,
        progress,
    )
    .await?;
    report.serving_home = serving_home;
    Ok(report)
}

/// Whether `home`'s own node runs: its service job, or the endpoint its
/// runtime record names.
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

/// The set-up folder other than `chosen` the service is defined for, when the
/// port answers with that folder's own id: past onboarding. The public status
/// names no home, so the definition says which folder serves.
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
    manager: Option<&NativeServiceManager>,
    serving_home: Option<&Path>,
    program: &ServiceProgram,
    progress: &mut dyn Progress,
) -> Result<Report<Prepared>> {
    if let Some(blocker) = newer_node(facts) {
        return Ok(blocked(blocker));
    }
    if let (HomeState::Broken(why), Some(serving)) = (facts.home.clone(), serving_home) {
        if choice.explicit {
            return Ok(Report {
                repaired: Vec::new(),
                outcome: Prepared::Decide(decision(&choice.home, why, serving).await?),
                serving_home: Some(serving.to_path_buf()),
                older_data: None,
            });
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
    if matches!(facts.home, HomeState::Broken(_)) {
        steps.push(RepairStep::SetAsideFolder);
    }
    let mut repairs = Repairs::new(progress, steps.len());
    for finding in actions.iter().filter_map(finding_for) {
        repairs.found(finding);
    }
    for action in &actions {
        repairs.step(step_for(action));
        let manager = manager.context("a service repair needs the service manager")?;
        // Only these write this run's program into a definition, and only
        // toward a home that is a directory: setup or `start` installs a
        // home that is not one.
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
                Err(blocker) => return Ok(repairs.report(Prepared::Blocked(blocker))),
            }
        } else {
            manager
        };
        if let Err(blocker) = apply(action, manager, installs_program).await {
            return Ok(repairs.report(Prepared::Blocked(blocker)));
        }
        if let ServiceAction::SwitchProgram { from, .. } = action {
            repairs.found(Finding::MovedToThisVersion { from: from.clone() });
        }
    }
    let outcome = match (&facts.home, started_from, manager) {
        (HomeState::Complete, Some(from), Some(manager)) => {
            repairs.step(RepairStep::WaitForNode);
            opens_after_switch(&choice.home, manager, from, &mut repairs).await?
        }
        (HomeState::Complete, None, Some(manager)) if replacing == Some(true) => {
            repairs.step(RepairStep::WaitForNode);
            let serving = serving_home.filter(|_| choice.explicit);
            opens_after_replacing(
                &choice.home,
                manager,
                serving,
                replaced.as_ref(),
                &mut repairs,
            )
            .await?
        }
        (HomeState::Absent, ..) => Prepared::NeedsSetup,
        (HomeState::Complete, ..) => Prepared::Ready,
        (HomeState::Broken(why), ..) => {
            repairs.step(RepairStep::SetAsideFolder);
            let (older, label) = match why {
                BrokenHome::OlderVersionData => {
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
                    Prepared::NeedsSetup
                }
                Err(blocker) => Prepared::Blocked(blocker),
            }
        }
    };
    let mut report = repairs.report(outcome);
    // A bundle at an unchanged path is this build's, so only what an older
    // node answered names the default home's data.
    report.older_data = replaced.or_else(|| {
        (!choice.explicit).then(|| OlderData {
            from: answered_from,
        })
    });
    Ok(report)
}

/// Whether this run's service action replaced the program or folder the
/// service ran, other than a switch from an answering node, and whether it
/// started the node. A passed folder is never set aside unasked, so it keeps
/// the handling it had before this run's action.
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

/// Waits for the node a replaced service started. Only older data it refuses
/// is set aside here; a question or any other failure is left to whatever
/// starts the node next (`start`, or the desktop's launch), as before this wait.
async fn opens_after_replacing(
    home: &Path,
    manager: &NativeServiceManager,
    serving_home: Option<&Path>,
    older_data: Option<&OlderData>,
    repairs: &mut Repairs<'_>,
) -> Result<Prepared> {
    // The action bootstrapped a new job, so any exit it records is this one's.
    let since = Since {
        platform: NativeServicePlatform::current()?,
        runs: None,
        started: true,
    };
    let kept = |kept_at| Finding::BrokenHome {
        why: BrokenHome::StoreKeyMissing,
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

fn after_replacing(waited: Started) -> Prepared {
    match waited {
        Started::NeedsSetup => Prepared::NeedsSetup,
        Started::Blocked(blocker @ Blocker::CannotSetAside { .. }) => Prepared::Blocked(blocker),
        Started::Answering(_) | Started::Decide(_) | Started::Blocked(_) => Prepared::Ready,
    }
}

/// The version this run's switch moved from, when the older node that
/// answered was this home's own service, and whether the switch started this
/// version's node. Only then is the home's data that version's.
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

/// A broken home set aside after a switch from its own older service is
/// that version's data; otherwise it keeps its specific reason.
fn broken_set_aside(
    why: BrokenHome,
    older: Option<Option<semver::Version>>,
    kept_at: PathBuf,
) -> Finding {
    match older {
        Some(from) => Finding::OlderDataSetAside { from, kept_at },
        None => Finding::BrokenHome { why, kept_at },
    }
}

/// Waits for the node a switch from an answering older node started. One
/// that keeps exiting or refuses the store cannot open the data the old
/// version left, so the home is set aside for setup, named after that version.
async fn opens_after_switch(
    home: &Path,
    manager: &NativeServiceManager,
    from: Option<semver::Version>,
    repairs: &mut Repairs<'_>,
) -> Result<Prepared> {
    // The switch bootstrapped a new job, so any exit it records is this one's.
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
        Started::Answering(_) => Prepared::Ready,
        Started::NeedsSetup => Prepared::NeedsSetup,
        Started::Decide(decision) => Prepared::Decide(decision),
        Started::Blocked(blocker) if cannot_open_after_switch(&blocker) => {
            match stop_and_set_aside(home, manager, &label, repairs).await {
                Ok(kept_at) => {
                    repairs.found(kept(kept_at));
                    Prepared::NeedsSetup
                }
                Err(blocker) => Prepared::Blocked(blocker),
            }
        }
        Started::Blocked(blocker) => Prepared::Blocked(blocker),
    })
}

/// A switched node that fails this way could not open the old data; any other
/// failure (a locked Keychain, a busy port, no answer) is reported as is.
fn cannot_open_after_switch(blocker: &Blocker) -> bool {
    matches!(
        blocker,
        Blocker::KeepsExiting { .. } | Blocker::StoreRefused { .. }
    )
}

async fn stop_and_set_aside(
    home: &Path,
    manager: &NativeServiceManager,
    label: &str,
    repairs: &mut Repairs<'_>,
) -> Result<PathBuf, Blocker> {
    apply(&ServiceAction::StopStale, manager, false).await?;
    repairs.extra_step(RepairStep::SetAsideFolder);
    set_aside_home_as(home, label.to_string()).await
}

async fn decision(home: &Path, why: BrokenHome, serving: &Path) -> Result<HomeDecision> {
    Ok(HomeDecision {
        home: home.to_path_buf(),
        why,
        default_home: tokio::task::spawn_blocking(default_home_ignoring_env).await??,
        serving_home: serving.to_path_buf(),
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
        // Reported once applied: the move may still be refused.
        ServiceAction::SwitchProgram { .. }
        | ServiceAction::RefreshLog { .. }
        | ServiceAction::Restart => None,
    }
}

/// Installs `home`'s service if none is defined, starts it unless a node
/// already serves this home, and waits for its GraphQL, naming why when it
/// does not answer. With `serving_home` (a passed folder, past onboarding:
/// the folder the node served before) a missing store key is a question, not
/// a set-aside.
pub async fn start(
    home: &Path,
    serving_home: Option<&Path>,
    older_data: Option<&OlderData>,
    progress: &mut dyn Progress,
) -> Result<Report<Started>> {
    let _lock = hold_lock(&repair_lock_path()?).await?;
    let mut repairs = Repairs::new(progress, 2);
    if let Some(graphql) = answering_graphql(home).await? {
        let server = answering_server(home, &graphql)?;
        let observed = observe_port(&server, read_initialized_did(home).await.as_deref()).await;
        return Ok(
            repairs.report(match hand_started(server.http_port, observed) {
                Some(blocker) => Started::Blocked(blocker),
                None => Started::Answering(graphql),
            }),
        );
    }
    let Some(manager) = local_service(home, &current_executable()?)? else {
        return Ok(repairs.report(Started::Blocked(Blocker::NoServiceManager {
            home: home.to_path_buf(),
        })));
    };
    let config = manager.config().clone();
    repairs.step(RepairStep::StartNode);
    let (running, runs) = match on_manager(&config, |manager| {
        if manager.installed_home()?.is_none() {
            manager.install()?;
        }
        let runs = manager.last_exit()?.map(|exit| exit.restarts);
        Ok((manager.status()?.running, runs))
    })
    .await
    {
        Ok(sample) => sample,
        Err(blocker) => return Ok(repairs.report(Started::Blocked(blocker))),
    };
    let mut started = false;
    if !running {
        let server = ServerConfig::standard(home.to_path_buf());
        let port = server.http_port;
        match observe_port(&server, read_initialized_did(home).await.as_deref()).await {
            PortObservation::NotListening => {
                if let Err(blocker) = start_job(&config).await {
                    return Ok(repairs.report(Started::Blocked(blocker)));
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
                return Ok(repairs.report(Started::Blocked(Blocker::HandStartedNode { port, home })))
            }
            PortObservation::NotGents => {
                return Ok(repairs.report(Started::Blocked(Blocker::PortInUse {
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
        why: BrokenHome::StoreKeyMissing,
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

/// Where the runtime behind `graphql` serves its status.
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

/// A node with another id on this home's port: a runtime record left behind
/// would otherwise read it as this home's own.
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

/// A runtime records where its GraphQL answers once it serves, and nothing
/// tells another process when, so this bounded wait reads that record again
/// and samples the service for a refusal or a crash loop.
/// A home set aside for a missing store key is named by `label` and reported
/// with `kept`. With `older_data`, older data the node refuses is set aside
/// under its name.
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
) -> Result<Started> {
    let config = manager.config();
    let deadline = Instant::now() + ENDPOINT_WAIT;
    let mut next_sample = Instant::now() + SAMPLE_EVERY;
    let mut watch = CrashLoopWatch::default();
    loop {
        if let Some(graphql) = answering_graphql(home).await? {
            return Ok(Started::Answering(graphql));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(Started::Blocked(Blocker::NoAnswer {
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
                Err(blocker) => return Ok(Started::Blocked(blocker)),
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
                                    Started::NeedsSetup
                                }
                                Err(blocker) => Started::Blocked(blocker),
                            },
                        );
                    }
                    return Ok(Started::Blocked(blocker_in(
                        home,
                        exit,
                        last_error(config).await,
                    )));
                }
                Some((_, OnRefusal::Ask(serving))) => {
                    return Ok(Started::Decide(
                        decision(home, BrokenHome::StoreKeyMissing, serving).await?,
                    ));
                }
                Some((_, OnRefusal::SetAside)) => {
                    return Ok(
                        match stop_and_set_aside(home, manager, label, repairs).await {
                            Ok(kept_at) => {
                                repairs.found(kept(kept_at));
                                Started::NeedsSetup
                            }
                            Err(blocker) => Started::Blocked(blocker),
                        },
                    );
                }
                None => {}
            }
            if let Some((exit, _)) =
                watch.observe(job_sample(false, job_loaded, last_exit.as_ref()))
            {
                return Ok(Started::Blocked(blocker_in(
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
    /// Asks first, naming the folder that was serving.
    Ask(&'a Path),
    SetAside,
    Block,
}

/// The service as `start` found it: the run count of its last recorded exit,
/// and whether `start` started it.
#[derive(Debug, Clone, Copy)]
struct Since {
    platform: NativeServicePlatform,
    runs: Option<u64>,
    started: bool,
}

/// What a sampled refusal means, or `None` for an exit from an earlier run.
/// launchd counts every run, so an exit from this start has a grown count.
/// systemd never restarts a refusal (`RestartPreventExitStatus`) and its
/// count tracks only automatic restarts, but the start runs `reset-failed`,
/// which clears earlier results, so any refusal after it is this run's.
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
    Some(match (exit.refusal()?, serving_home) {
        (ServiceRefusal::Store(IncompatibleStoreKind::MissingStoreKey), Some(serving)) => {
            OnRefusal::Ask(serving)
        }
        (ServiceRefusal::Store(IncompatibleStoreKind::MissingStoreKey), None) => {
            OnRefusal::SetAside
        }
        _ => OnRefusal::Block,
    })
}

/// The data a fresh store refusal says this build cannot open, the store's
/// on-disk markers first, since one exit code covers several kinds.
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

/// Data this build cannot open, which is set aside so setup starts fresh: any
/// store it does not recognize, including one in an unknown format. Only a
/// store that may be a newer build's (`ForeignVersion`) is never touched, and
/// a missing key keeps its own handling.
fn cannot_open(kind: IncompatibleStoreKind) -> bool {
    !matches!(
        kind,
        IncompatibleStoreKind::ForeignVersion | IncompatibleStoreKind::MissingStoreKey
    )
}

/// Whether a refusal of `kind` that a caller observed, such as the desktop's
/// launch, sets `choice` aside with [`set_aside_refused`].
pub fn sets_aside_refused(choice: &HomeChoice, kind: IncompatibleStoreKind) -> bool {
    !choice.explicit && cannot_open(kind)
}

/// Stops `choice`'s service and sets the home aside as older data, for a
/// refusal a caller observed (see [`sets_aside_refused`]); setup follows.
pub async fn set_aside_refused(
    choice: &HomeChoice,
    progress: &mut dyn Progress,
) -> Result<Report<Prepared>> {
    let _lock = hold_lock(&repair_lock_path()?).await?;
    let mut repairs = Repairs::new(progress, 1);
    repairs.step(RepairStep::StopOldNode);
    let Some(manager) = local_service(&choice.home, &current_executable()?)? else {
        return Ok(repairs.report(Prepared::Blocked(Blocker::NoServiceManager {
            home: choice.home.clone(),
        })));
    };
    let outcome =
        match stop_and_set_aside(&choice.home, &manager, &older_label(None), &mut repairs).await {
            Ok(kept_at) => {
                repairs.found(Finding::OlderDataSetAside {
                    from: None,
                    kept_at,
                });
                Prepared::NeedsSetup
            }
            Err(blocker) => Prepared::Blocked(blocker),
        };
    Ok(repairs.report(outcome))
}

/// Whether `choice` is the default home and its own service, now stopped,
/// last refused its data as an older version's. A passed folder is never
/// set aside unasked.
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

/// [`blocker_for_exit`], naming a store refusal the log has no line for by
/// its kind rather than its exit code.
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

/// A service the desktop installed before the shared log wrote its errors here.
async fn last_error(config: &NativeServiceConfig) -> Option<String> {
    let legacy = config
        .user_home
        .join("Library/Application Support/gents/desktop/logs/runtime-errors.log");
    read_last_error(&config.user_home, Some(&legacy)).await
}

pub(crate) async fn answering_graphql(home: &Path) -> Result<Option<GraphqlEndpoint>> {
    match read_runtime_state(home)? {
        Some(state) if graphql_endpoint_available(&state.graphql).await => Ok(Some(
            crate::resolve_graphql_endpoint(Some(&state.graphql), Some(home))?,
        )),
        _ => Ok(None),
    }
}

/// `None` where this host has no service manager Gents can install into.
fn local_service(home: &Path, executable: &Path) -> Result<Option<NativeServiceManager>> {
    if NativeServicePlatform::current().is_err() {
        return Ok(None);
    }
    let config = NativeServiceConfig::new(home.to_path_buf(), executable.to_path_buf())?;
    Ok(Some(NativeServiceManager::new(config)?))
}

/// The program `gents` runs as, which the CLI's service runs too.
pub fn current_executable() -> Result<PathBuf> {
    std::env::current_exe().context("resolving the gents executable")
}

/// The CLI's [`ServiceProgram`]: the `gents` running now.
pub fn current_program() -> Result<PathBuf, Blocker> {
    current_executable()
        .and_then(|path| std::fs::canonicalize(&path).context("resolving the gents executable"))
        .map_err(|error| Blocker::ServiceUpdateFailed {
            reason: format!("{error:#}"),
        })
}

fn repair_lock_path() -> Result<PathBuf> {
    Ok(gents::store::local::home::gents_config_dir()?.join("repair.lock"))
}

/// Serializes repairs across processes: the desktop at login and a terminal
/// running `gents` must not both stop or set aside the same node and folder.
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

fn blocked(blocker: Blocker) -> Report<Prepared> {
    Report {
        repaired: Vec::new(),
        outcome: Prepared::Blocked(blocker),
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

    /// A step the plan could not count: it follows what a wait observed.
    fn extra_step(&mut self, step: RepairStep) {
        self.total += 1;
        self.step(step);
    }

    fn report<T>(self, outcome: T) -> Report<T> {
        Report {
            repaired: self.repaired,
            outcome,
            serving_home: None,
            older_data: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_service::ServiceExit;
    use crate::repair::home::classify_home;
    use crate::repair::port::PortObservation;
    use crate::repair::service::{this_version, NodeVersion, ServiceFacts};
    use std::fs;
    use std::path::Path;

    #[derive(Default)]
    struct Seen {
        findings: Vec<Finding>,
    }

    impl Progress for Seen {
        fn finding(&mut self, finding: &Finding) {
            self.findings.push(finding.clone());
        }

        fn step(&mut self, _index: usize, _total: usize, _step: RepairStep) {}
    }

    fn unserviced(home: &Path) -> ServiceFacts {
        ServiceFacts {
            definition_home: None,
            executable_present: true,
            log_current: true,
            job_loaded: false,
            job_running: false,
            job_failed: false,
            port: PortObservation::NotListening,
            home: classify_home(home, None).unwrap(),
            was_serving: false,
            service_move: ServiceMove::BeforeSetup,
            node_version: None,
            foreign_program: false,
            managed_update: false,
            previous_version: None,
        }
    }

    fn unreadable(home: &Path) {
        fs::create_dir_all(home).unwrap();
        fs::write(home.join("init.json"), "{").unwrap();
    }

    fn choice(home: &Path, explicit: bool) -> HomeChoice {
        HomeChoice {
            home: home.to_path_buf(),
            explicit,
        }
    }

    fn never_resolved() -> ServiceProgram {
        Arc::new(|| panic!("only a reinstall resolves the service program"))
    }

    #[tokio::test]
    async fn a_repair_with_nothing_to_install_never_resolves_the_service_program() {
        static RESOLVED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let cannot_install: ServiceProgram = Arc::new(|| {
            RESOLVED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(Blocker::ServiceUpdateFailed {
                reason: "Gents is running from a disk image".into(),
            })
        });
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        fs::create_dir_all(&home).unwrap();
        let healthy = ServiceFacts {
            definition_home: Some(home.clone()),
            job_loaded: true,
            job_running: true,
            port: PortObservation::Gents {
                identity: ServedIdentity::Matches,
                payload: serde_json::Value::Null,
            },
            home: HomeState::Complete,
            node_version: Some(NodeVersion::Same),
            ..unserviced(&home)
        };
        let report = settle(
            &choice(&home, false),
            &healthy,
            None,
            None,
            &cannot_install,
            &mut Seen::default(),
        )
        .await
        .unwrap();
        assert!(
            matches!(report.outcome, Prepared::Ready),
            "{:?}",
            report.outcome
        );
        assert_eq!(RESOLVED.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    fn answering_complete(home: &Path, node_version: NodeVersion) -> ServiceFacts {
        ServiceFacts {
            definition_home: Some(home.to_path_buf()),
            job_loaded: true,
            job_running: true,
            port: PortObservation::Gents {
                identity: ServedIdentity::Matches,
                payload: serde_json::Value::Null,
            },
            home: HomeState::Complete,
            node_version: Some(node_version),
            ..unserviced(home)
        }
    }

    #[tokio::test]
    async fn a_newer_node_is_left_alone_and_named() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        fs::create_dir_all(&home).unwrap();
        let newer = semver::Version::new(1, 1, 0);
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, false),
            &answering_complete(&home, NodeVersion::Newer(newer)),
            None,
            None,
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        let ours = this_version().to_string();
        match report.outcome {
            Prepared::Blocked(blocker) => {
                assert_eq!(
                    blocker,
                    Blocker::NewerNode {
                        running: "1.1.0".into(),
                        ours: ours.clone()
                    }
                );
                assert_eq!(
                    blocker.to_string(),
                    format!("Your local node runs Gents 1.1.0, newer than this Gents ({ours}). Open Gents 1.1.0 or newer, then try again.")
                );
            }
            other => panic!("expected the newer node to block, got {other:?}"),
        }
        assert!(seen.findings.is_empty(), "{:?}", seen.findings);
        assert!(home.is_dir());
    }

    #[tokio::test]
    async fn the_same_version_under_another_program_needs_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        fs::create_dir_all(&home).unwrap();
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, false),
            &answering_complete(&home, NodeVersion::Same),
            None,
            None,
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        assert!(
            matches!(report.outcome, Prepared::Ready),
            "{:?}",
            report.outcome
        );
        assert!(seen.findings.is_empty(), "{:?}", seen.findings);
    }

    #[test]
    fn a_switch_names_the_version_it_moved_from_once_applied() {
        let switch = |from, answered, start_after| ServiceAction::SwitchProgram {
            from,
            answered,
            start_after,
        };
        let v021 = semver::Version::new(0, 21, 0);
        assert_eq!(finding_for(&switch(Some(v021.clone()), true, true)), None);
        assert_eq!(
            Finding::MovedToThisVersion {
                from: Some(v021.clone())
            }
            .to_string(),
            "Gents moved your local node from version 0.21.0 to this version"
        );
        assert_eq!(
            Finding::MovedToThisVersion { from: None }.to_string(),
            "Gents moved your local node to this version"
        );
        assert_eq!(
            step_for(&switch(None, false, false)),
            RepairStep::UpdateService
        );
    }

    #[test]
    fn only_a_started_switch_from_an_answering_node_waits_to_set_aside() {
        let v021 = semver::Version::new(0, 21, 0);
        let switch = |from: Option<semver::Version>, answered, start_after| {
            vec![ServiceAction::SwitchProgram {
                from,
                answered,
                start_after,
            }]
        };
        assert_eq!(
            answered_switch(&switch(Some(v021.clone()), true, true)),
            Some((Some(v021.clone()), true))
        );
        assert_eq!(
            answered_switch(&switch(None, true, true)),
            Some((None, true))
        );
        // A silent job's version is unknown, so only a refusal of older data
        // is set aside after it (see `replaced_service`), never a plain crash.
        assert_eq!(answered_switch(&switch(None, false, true)), None);
        assert_eq!(
            answered_switch(&switch(Some(v021.clone()), true, false)),
            Some((Some(v021), false))
        );
        assert_eq!(answered_switch(&[ServiceAction::StopStale]), None);
    }

    #[test]
    fn a_silent_switch_a_move_or_a_reinstall_replaces_the_service() {
        let default = choice(Path::new("/Users/me/.gents"), false);
        let passed = choice(Path::new("/Users/me/work"), true);
        let silent = |start_after| ServiceAction::SwitchProgram {
            from: None,
            answered: false,
            start_after,
        };
        assert_eq!(replaced_service(&default, &[silent(true)]), Some(true));
        assert_eq!(replaced_service(&default, &[silent(false)]), Some(false));
        let moved = ServiceAction::MoveFrom {
            from: PathBuf::from("/Users/me/other"),
            start_after: true,
            replace_program: true,
        };
        assert_eq!(
            replaced_service(&default, std::slice::from_ref(&moved)),
            Some(true)
        );
        // A passed --home the service moved to is never set aside unasked.
        assert_eq!(replaced_service(&passed, &[moved]), None);
        assert_eq!(replaced_service(&passed, &[silent(true)]), None);
        assert_eq!(
            replaced_service(&default, &[ServiceAction::Reinstall { start_after: false }]),
            Some(false)
        );
        // An answering node's switch keeps its own rule (`opens_after_switch`).
        let answered = ServiceAction::SwitchProgram {
            from: None,
            answered: true,
            start_after: true,
        };
        assert_eq!(replaced_service(&default, &[answered]), None);
        for untouched in [
            ServiceAction::StopStale,
            ServiceAction::Restart,
            ServiceAction::RefreshLog { restart: true },
        ] {
            assert_eq!(
                replaced_service(&default, std::slice::from_ref(&untouched)),
                None,
                "{untouched:?}"
            );
        }
        assert_eq!(replaced_service(&default, &[]), None);
    }

    #[test]
    fn data_this_build_cannot_open_is_set_aside_after_a_replaced_service() {
        use IncompatibleStoreKind::*;
        for unopenable in [
            LegacyRocksDb,
            LegacyLark,
            UnknownLineage,
            InsecureKey,
            UnencryptedStore,
            OtherVersionSchema,
            UnsupportedOrCorrupt,
        ] {
            assert!(cannot_open(unopenable), "{unopenable:?}");
        }
        for kept in [ForeignVersion, MissingStoreKey] {
            assert!(!cannot_open(kept), "{kept:?}");
        }
    }

    #[tokio::test]
    async fn a_refusal_is_read_from_the_store_first_then_from_the_exit_code() {
        use crate::native_service::{
            FOREIGN_STORE_EXIT_CODE, INCOMPATIBLE_STORE_EXIT_CODE, INSECURE_KEY_EXIT_CODE,
            MISSING_STORE_KEY_EXIT_CODE,
        };
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        let data = default_data_dir(&home);
        fs::create_dir_all(&data).unwrap();
        let refused = |code| {
            let home = home.clone();
            async move { refused_older_data(&home, &exited(code, 1)).await }
        };
        // An rc1 home: no provenance marker, which the node exits 65 for.
        assert_eq!(
            refused(INCOMPATIBLE_STORE_EXIT_CODE).await,
            Some(IncompatibleStoreKind::UnknownLineage)
        );
        assert_eq!(
            refused(INSECURE_KEY_EXIT_CODE).await,
            Some(IncompatibleStoreKind::InsecureKey)
        );
        assert_eq!(refused(FOREIGN_STORE_EXIT_CODE).await, None);
        assert_eq!(refused(MISSING_STORE_KEY_EXIT_CODE).await, None);
        assert_eq!(refused(1).await, None, "a plain crash");
        fs::write(data.join("CURRENT"), "MANIFEST-000001").unwrap();
        assert_eq!(
            refused(INCOMPATIBLE_STORE_EXIT_CODE).await,
            Some(IncompatibleStoreKind::LegacyRocksDb)
        );
        fs::remove_file(data.join("CURRENT")).unwrap();
        fs::write(data.join("MANIFEST"), "garbage").unwrap();
        assert_eq!(
            refused(INCOMPATIBLE_STORE_EXIT_CODE).await,
            Some(IncompatibleStoreKind::UnsupportedOrCorrupt),
            "a store in an unknown format is set aside too"
        );
    }

    #[test]
    fn a_wait_after_a_replaced_service_acts_only_on_what_it_set_aside() {
        assert!(matches!(
            after_replacing(Started::NeedsSetup),
            Prepared::NeedsSetup
        ));
        let cannot = Blocker::CannotSetAside {
            home: PathBuf::from("/Users/me/.gents"),
            reason: "permission denied".into(),
        };
        assert!(matches!(
            after_replacing(Started::Blocked(cannot.clone())),
            Prepared::Blocked(blocker) if blocker == cannot
        ));
        // Left to the start that follows, as before this wait.
        for left in [
            Blocker::StoreRefused {
                kind: IncompatibleStoreKind::ForeignVersion,
                message: "written by a different Gents version".into(),
            },
            Blocker::KeepsExiting {
                code: Some(1),
                last_error: None,
            },
            Blocker::KeychainUnavailable,
        ] {
            assert!(
                matches!(
                    after_replacing(Started::Blocked(left.clone())),
                    Prepared::Ready
                ),
                "{left:?}"
            );
        }
        assert!(matches!(
            after_replacing(Started::Answering(GraphqlEndpoint::anonymous(
                "http://127.0.0.1:1/api/v0/graphql"
            ))),
            Prepared::Ready
        ));
    }

    #[tokio::test]
    async fn older_data_is_set_aside_named_for_the_replaced_programs_version() {
        let rc1 = semver::Version::parse("1.0.0-rc1").unwrap();
        for (from, prefix) in [
            (Some(rc1.clone()), ".gents.from-1.0.0-rc1-"),
            (None, ".gents.from-older-"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().join(".gents");
            fs::create_dir_all(default_data_dir(&home)).unwrap();
            let kept_at = set_aside_home_as(&home, older_label(from.as_ref()))
                .await
                .unwrap();
            let name = kept_at.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(prefix), "{name}");
            assert!(!home.exists());
            assert!(kept_at.join("data").is_dir());
            let note = Finding::OlderDataSetAside {
                from: from.clone(),
                kept_at: kept_at.clone(),
            }
            .to_string();
            assert!(note.ends_with(&format!("so it was set aside at {}", kept_at.display())));
        }
    }

    #[test]
    fn a_refused_store_with_no_logged_error_is_named_by_its_kind() {
        let home = Path::new("/Users/me/.gents");
        let exit = exited(crate::native_service::INCOMPATIBLE_STORE_EXIT_CODE, 1);
        assert_eq!(
            blocker_in(home, &exit, None),
            Blocker::StoreRefused {
                kind: IncompatibleStoreKind::UnknownLineage,
                message: IncompatibleStore {
                    kind: IncompatibleStoreKind::UnknownLineage,
                    data_path: default_data_dir(home),
                }
                .to_string(),
            }
        );
        assert_eq!(
            blocker_in(home, &exit, Some("logged".into())),
            Blocker::StoreRefused {
                kind: IncompatibleStoreKind::UnknownLineage,
                message: "logged".into(),
            }
        );
        let insecure = exited(crate::native_service::INSECURE_KEY_EXIT_CODE, 1);
        assert!(blocker_in(home, &insecure, None)
            .to_string()
            .contains("/Users/me/.gents/keys"));
        assert_eq!(
            blocker_in(home, &exited(1, 1), None),
            Blocker::KeepsExiting {
                code: Some(1),
                last_error: None
            }
        );
    }

    #[test]
    fn only_an_observed_older_refusal_of_the_default_home_is_set_aside() {
        use IncompatibleStoreKind::*;
        let default = choice(Path::new("/Users/me/.gents"), false);
        let passed = choice(Path::new("/Users/me/work"), true);
        for older in [
            UnknownLineage,
            InsecureKey,
            OtherVersionSchema,
            LegacyRocksDb,
            UnsupportedOrCorrupt,
        ] {
            assert!(sets_aside_refused(&default, older), "{older:?}");
            assert!(!sets_aside_refused(&passed, older), "{older:?}");
        }
        for kept in [ForeignVersion, MissingStoreKey] {
            assert!(!sets_aside_refused(&default, kept), "{kept:?}");
        }
    }

    #[tokio::test]
    async fn only_the_default_homes_stopped_service_refusing_older_data_marks_it_older() {
        use crate::native_service::{FOREIGN_STORE_EXIT_CODE, INCOMPATIBLE_STORE_EXIT_CODE};
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        fs::create_dir_all(default_data_dir(&home)).unwrap();
        let refused = exited(INCOMPATIBLE_STORE_EXIT_CODE, 3);
        let default = choice(&home, false);
        assert!(older_refused(&default, Some(&refused), false).await);
        assert!(
            !older_refused(&choice(&home, true), Some(&refused), false).await,
            "a passed --home keeps today's handling"
        );
        assert!(
            !older_refused(&default, Some(&refused), true).await,
            "a running node is left alone"
        );
        assert!(!older_refused(&default, Some(&exited(FOREIGN_STORE_EXIT_CODE, 3)), false).await);
        assert!(!older_refused(&default, Some(&exited(1, 3)), false).await);
        assert!(!older_refused(&default, None, false).await);
    }

    #[tokio::test]
    async fn a_same_path_upgrade_sets_the_older_default_home_aside_without_asking() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        fs::create_dir_all(default_data_dir(&home)).unwrap();
        // No service action: the program at the defined path is this one.
        let facts = ServiceFacts {
            home: HomeState::Broken(BrokenHome::OlderVersionData),
            ..unserviced(&home)
        };
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, false),
            &facts,
            None,
            None,
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        assert!(
            matches!(report.outcome, Prepared::NeedsSetup),
            "{:?}",
            report.outcome
        );
        let kept_at = match seen.findings.as_slice() {
            [Finding::OlderDataSetAside {
                from: None,
                kept_at,
            }] => kept_at.clone(),
            other => panic!("expected the older data set aside, got {other:?}"),
        };
        let name = kept_at.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".gents.from-older-"), "{name}");
        assert!(!home.exists());
        assert!(kept_at.join("data").is_dir());
    }

    #[tokio::test]
    async fn a_start_after_prepare_sets_aside_older_data_only_for_the_default_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        fs::create_dir_all(&home).unwrap();
        let facts = ServiceFacts {
            home: HomeState::Complete,
            ..unserviced(&home)
        };
        for (explicit, older_data) in [(false, Some(OlderData { from: None })), (true, None)] {
            let report = settle(
                &choice(&home, explicit),
                &facts,
                None,
                None,
                &never_resolved(),
                &mut Seen::default(),
            )
            .await
            .unwrap();
            assert!(
                matches!(report.outcome, Prepared::Ready),
                "{:?}",
                report.outcome
            );
            assert_eq!(report.older_data, older_data, "explicit: {explicit}");
        }
    }

    #[test]
    fn a_broken_home_its_own_older_service_answered_on_is_that_versions_data() {
        let v019 = semver::Version::new(0, 19, 0);
        let kept_at = PathBuf::from("/Users/me/.gents.from-0.19.0-STAMP");
        let finding = broken_set_aside(
            BrokenHome::InitConfigUnreadable,
            Some(Some(v019.clone())),
            kept_at.clone(),
        );
        assert_eq!(
            finding,
            Finding::OlderDataSetAside {
                from: Some(v019.clone()),
                kept_at: kept_at.clone()
            }
        );
        assert_eq!(
            finding.to_string(),
            "this version couldn't open the data from Gents 0.19.0, so it was set aside at /Users/me/.gents.from-0.19.0-STAMP"
        );
        assert_eq!(aside_label(Some(&v019)), "from-0.19.0");
        assert_eq!(
            broken_set_aside(BrokenHome::SetupStoppedMidway, None, kept_at.clone()),
            Finding::BrokenHome {
                why: BrokenHome::SetupStoppedMidway,
                kept_at
            }
        );
    }

    #[tokio::test]
    async fn an_older_node_serving_another_folder_does_not_relabel_this_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        unreadable(&home);
        // A hand-started v0.19 node, or one serving another folder: no switch.
        let facts = ServiceFacts {
            port: PortObservation::Gents {
                identity: ServedIdentity::Different {
                    node_did: None,
                    home: None,
                },
                payload: serde_json::json!({ "version": "0.19.0" }),
            },
            node_version: Some(NodeVersion::Older(Some(semver::Version::new(0, 19, 0)))),
            ..unserviced(&home)
        };
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, false),
            &facts,
            None,
            None,
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        assert!(
            matches!(report.outcome, Prepared::NeedsSetup),
            "{:?}",
            report.outcome
        );
        let kept_at = kept_at(&seen.findings);
        let name = kept_at.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".gents.broken-"), "{name}");
        assert!(kept_at.join("init.json").exists());
    }

    #[test]
    fn a_switched_node_that_keeps_exiting_or_refuses_its_store_could_not_open_the_old_data() {
        assert!(cannot_open_after_switch(&Blocker::KeepsExiting {
            code: Some(1),
            last_error: Some("default behavior \"\" is not canonical".into()),
        }));
        assert!(cannot_open_after_switch(&Blocker::StoreRefused {
            kind: IncompatibleStoreKind::UnknownLineage,
            message: "exited with code 65".into(),
        }));
        for kept in [
            Blocker::KeychainUnavailable,
            Blocker::NoAnswer {
                seconds: 60,
                last_error: None,
            },
            Blocker::PortInUse {
                port: 9191,
                holder: None,
            },
            Blocker::BackgroundApprovalPending { detail: None },
        ] {
            assert!(!cannot_open_after_switch(&kept), "{kept:?}");
        }
        let finding = Finding::OlderDataSetAside {
            from: Some(semver::Version::new(0, 19, 0)),
            kept_at: PathBuf::from("/Users/me/.gents.from-0.19.0-2026-11-11-120000"),
        };
        assert_eq!(
            finding.to_string(),
            "this version couldn't open the data from Gents 0.19.0, so it was set aside at /Users/me/.gents.from-0.19.0-2026-11-11-120000"
        );
        assert_eq!(
            Finding::OlderDataSetAside {
                from: None,
                kept_at: PathBuf::from("/Users/me/.gents.broken-STAMP"),
            }
            .to_string(),
            "this version couldn't open the data from an older Gents version, so it was set aside at /Users/me/.gents.broken-STAMP"
        );
    }

    fn kept_at(findings: &[Finding]) -> PathBuf {
        match findings {
            [Finding::BrokenHome {
                why: BrokenHome::InitConfigUnreadable,
                kept_at,
            }] => kept_at.clone(),
            other => panic!("expected one set-aside finding, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_absent_home_needs_setup() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, true),
            &unserviced(&home),
            None,
            Some(Path::new("/Users/me/serving")),
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, Prepared::NeedsSetup));
        assert!(report.repaired.is_empty());
        assert!(seen.findings.is_empty());
    }

    #[tokio::test]
    async fn a_broken_default_home_is_set_aside_without_asking() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        unreadable(&home);
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, false),
            &unserviced(&home),
            None,
            Some(Path::new("/Users/me/serving")),
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, Prepared::NeedsSetup));
        let kept_at = kept_at(&seen.findings);
        assert_eq!(report.repaired, seen.findings);
        assert!(!home.exists());
        assert!(kept_at.join("init.json").exists());
        assert_eq!(
            seen.findings[0].to_string(),
            format!(
                "its settings file (init.json) can't be read; it was set aside as {}",
                kept_at.display()
            )
        );
    }

    #[tokio::test]
    async fn a_broken_explicit_home_after_onboarding_asks_first() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("work");
        unreadable(&home);
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, true),
            &unserviced(&home),
            None,
            Some(Path::new("/Users/me/serving")),
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        match report.outcome {
            Prepared::Decide(decision) => {
                assert_eq!(decision.home, home);
                assert_eq!(decision.why, BrokenHome::InitConfigUnreadable);
                assert_eq!(decision.serving_home, Path::new("/Users/me/serving"));
            }
            other => panic!("expected a decision, got {other:?}"),
        }
        assert!(seen.findings.is_empty(), "{:?}", seen.findings);
        assert!(home.join("init.json").exists());
    }

    #[tokio::test]
    async fn a_broken_explicit_home_during_onboarding_is_set_aside() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("work");
        unreadable(&home);
        let mut seen = Seen::default();
        let report = settle(
            &choice(&home, true),
            &unserviced(&home),
            None,
            None,
            &never_resolved(),
            &mut seen,
        )
        .await
        .unwrap();
        assert!(matches!(report.outcome, Prepared::NeedsSetup));
        kept_at(&seen.findings);
        assert!(!home.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_prepares_serialize() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".gents");
        unreadable(&home);
        let lock = temp.path().join("config/gents/repair.lock");
        let mut closed = ServerConfig::standard(home.clone());
        closed.http_port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let run = || {
            let (lock, choice, server) = (lock.clone(), choice(&home, false), closed.clone());
            tokio::spawn(async move {
                let mut seen = Seen::default();
                prepare_with_lock(
                    &lock,
                    None,
                    &server,
                    &choice,
                    false,
                    &never_resolved(),
                    ServiceMove::BeforeSetup,
                    &mut seen,
                )
                .await
                .map(|report| (report.outcome, seen.findings))
            })
        };
        let (first, second) = (run(), run());
        let mut findings = Vec::new();
        for task in [first, second] {
            let (outcome, seen) = task.await.unwrap().unwrap();
            assert!(matches!(outcome, Prepared::NeedsSetup), "{outcome:?}");
            findings.extend(seen);
        }
        kept_at(&findings);
    }

    #[tokio::test]
    async fn past_onboarding_is_another_set_up_folder_answering_with_its_own_id() {
        let temp = tempfile::tempdir().unwrap();
        let served = temp.path().join("served");
        fs::create_dir_all(&served).unwrap();
        fs::write(
            served.join("init.json"),
            serde_json::json!({ "node_did": "did:key:zserved" }).to_string(),
        )
        .unwrap();
        // The public status names no home: the definition says which folder serves.
        let answering = |did: &str| PortObservation::Gents {
            identity: crate::repair::port::ServedIdentity::Different {
                node_did: Some(did.into()),
                home: None,
            },
            payload: serde_json::json!({ "node_did": did }),
        };
        let chosen = temp.path().join("work");
        let defined = Some(served.as_path());
        assert_eq!(
            serving_home(defined, &answering("did:key:zserved"), &chosen).await,
            Some(served.clone())
        );
        assert_eq!(
            serving_home(defined, &answering("did:key:zother"), &chosen).await,
            None
        );
        assert_eq!(
            serving_home(defined, &answering("did:key:zserved"), &served).await,
            None
        );
        assert_eq!(
            serving_home(None, &answering("did:key:zserved"), &chosen).await,
            None
        );
        assert_eq!(
            serving_home(defined, &PortObservation::NotListening, &chosen).await,
            None
        );
    }

    fn exited(code: i32, restarts: u64) -> ServiceExit {
        ServiceExit {
            reason: format!("exited with code {code}"),
            restarts,
            clean: false,
            code: Some(code),
        }
    }

    #[test]
    fn only_a_refusal_after_this_start_counts_and_a_passed_folder_asks_first() {
        use NativeServicePlatform::{Linux, Macos};
        let serving = Some(Path::new("/Users/me/serving"));
        let missing_key = exited(crate::native_service::MISSING_STORE_KEY_EXIT_CODE, 4);
        // launchd counts every run, so an exit from this start has grown the count.
        assert_eq!(
            on_refusal(&missing_key, Macos, Some(4), true, None),
            None,
            "stale"
        );
        assert_eq!(
            on_refusal(&missing_key, Macos, Some(3), true, None),
            Some(OnRefusal::SetAside)
        );
        assert_eq!(
            on_refusal(&missing_key, Macos, None, false, None),
            Some(OnRefusal::SetAside)
        );
        assert_eq!(
            on_refusal(&missing_key, Macos, Some(3), true, serving),
            Some(OnRefusal::Ask(Path::new("/Users/me/serving")))
        );
        // systemd never restarts a refusal, and its start cleared older results.
        assert_eq!(
            on_refusal(&missing_key, Linux, Some(4), true, None),
            Some(OnRefusal::SetAside)
        );
        assert_eq!(on_refusal(&missing_key, Linux, Some(4), false, None), None);
        let locked = exited(crate::native_service::STORE_KEY_UNAVAILABLE_EXIT_CODE, 4);
        assert_eq!(
            on_refusal(&locked, Macos, Some(3), true, serving),
            Some(OnRefusal::Block)
        );
        assert_eq!(
            on_refusal(&exited(1, 9), Linux, None, true, serving),
            None,
            "not a refusal"
        );
    }

    #[test]
    fn the_answering_runtime_is_checked_where_its_record_points() {
        let server = answering_server(
            Path::new("/srv/home"),
            &GraphqlEndpoint::anonymous("http://127.0.0.1:4321/api/v0/graphql"),
        )
        .unwrap();
        assert_eq!(server.status_url(), "http://127.0.0.1:4321/status");
    }

    #[test]
    fn an_answering_record_on_a_port_with_another_id_is_a_hand_started_node() {
        let gents = |identity| PortObservation::Gents {
            identity,
            payload: serde_json::Value::Null,
        };
        let other = gents(ServedIdentity::Different {
            node_did: Some("did:key:zother".into()),
            home: Some("/srv/other".into()),
        });
        assert_eq!(
            hand_started(9, other),
            Some(Blocker::HandStartedNode {
                port: 9,
                home: Some("/srv/other".into())
            })
        );
        for observed in [
            gents(ServedIdentity::Matches),
            gents(ServedIdentity::Different {
                node_did: None,
                home: None,
            }),
            PortObservation::NotListening,
            PortObservation::NotGents,
        ] {
            assert_eq!(hand_started(9, observed.clone()), None, "{observed:?}");
        }
    }

    #[test]
    fn an_unapproved_background_item_reads_as_the_approval_text() {
        assert_eq!(
            Blocker::BackgroundApprovalPending { detail: None }.to_string(),
            crate::native_service::BACKGROUND_APPROVAL_REQUIRED
        );
    }
}
