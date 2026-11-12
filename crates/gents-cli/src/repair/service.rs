use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::native_service::{
    CommandRunner, NativeServiceConfig, NativeServiceManager, StartOutcome,
};
use crate::repair::home::HomeState;
use crate::repair::port::{observe_port, read_initialized_did, PortObservation, ServedIdentity};
use crate::repair::{Blocker, ServiceProgram};
use crate::server_host::ServerConfig;

#[derive(Debug, Clone)]
pub struct ServiceFacts {
    pub definition_home: Option<PathBuf>,
    pub executable_present: bool,
    pub log_current: bool,
    pub job_loaded: bool,
    pub job_running: bool,
    /// The supervisor gave up on the job (a systemd unit left `failed`),
    /// which a deliberate stop does not leave.
    pub job_failed: bool,
    pub port: PortObservation,
    pub home: HomeState,
    /// The chosen folder answered before this run moved its node away.
    pub was_serving: bool,
    pub service_move: ServiceMove,
    /// The version of the Gents node answering on the home's port, against
    /// this build; `None` when no Gents node answers.
    pub node_version: Option<NodeVersion>,
    /// The service runs a program other than the one this run would install.
    /// Known only for a service defined for another folder, or this home's
    /// service whose job is silent.
    pub foreign_program: bool,
    /// A managed update selected the service's program, or its handoff is
    /// pending: automatic repairs leave the program to that owner.
    pub managed_update: bool,
    /// The version the app bundle holding the service's program names
    /// (`CFBundleShortVersionString`), when older than this build: a bare
    /// program names none.
    pub previous_version: Option<semver::Version>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeVersion {
    /// `None` when the node reports no version, or none that parses.
    Older(Option<semver::Version>),
    Same,
    Newer(semver::Version),
}

/// How `running`, a node's reported version, compares with `ours`. Nodes
/// that predate the field report none, so a missing or unparseable version
/// is older.
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

/// The version of the Gents node `port` answered with, against this build.
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

/// The version the `.app` bundle holding `program` names, when older than this
/// build. The bundle may already have been replaced by this build or a newer one.
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

/// A newer node answering is left alone: this build may not open what it wrote.
pub fn newer_node(facts: &ServiceFacts) -> Option<Blocker> {
    match &facts.node_version {
        Some(NodeVersion::Newer(running)) => Some(Blocker::NewerNode {
            running: running.to_string(),
            ours: this_version().to_string(),
        }),
        _ => None,
    }
}

/// When a service defined for another folder moves to a chosen folder that
/// still needs setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceMove {
    /// The desktop's wizard installs its own service, so the old one is
    /// removed before setup.
    BeforeSetup,
    /// The CLI repairs again after setup, so the old node serves until then.
    AfterSetup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceAction {
    /// `replace_program` installs this run's program instead of carrying
    /// the old one.
    MoveFrom {
        from: PathBuf,
        start_after: bool,
        replace_program: bool,
    },
    StopStale,
    Reinstall {
        start_after: bool,
    },
    /// Carries the definition onto this run's program, as `Reinstall` does.
    /// `answered`: an older node (`from`, when it reported a version) served
    /// the home, so a start that cannot open its data sets the home aside.
    /// Otherwise the job was silent and its version unknown.
    SwitchProgram {
        from: Option<semver::Version>,
        answered: bool,
        start_after: bool,
    },
    RefreshLog {
        restart: bool,
    },
    Restart,
}

pub fn plan_service(facts: &ServiceFacts, chosen: &Path) -> Vec<ServiceAction> {
    let Some(definition_home) = &facts.definition_home else {
        return Vec::new();
    };
    if definition_home != chosen {
        if facts.service_move == ServiceMove::AfterSetup && facts.home != HomeState::Complete {
            return Vec::new();
        }
        return vec![ServiceAction::MoveFrom {
            from: definition_home.clone(),
            start_after: (facts.job_loaded || facts.was_serving)
                && facts.home == HomeState::Complete,
            replace_program: facts.foreign_program,
        }];
    }
    let complete = facts.home == HomeState::Complete;
    if !facts.executable_present {
        return vec![ServiceAction::Reinstall {
            start_after: facts.job_loaded && complete,
        }];
    }
    // An older node the job serves, or a job on another program that is
    // stopped or crash-looping; a booting job is not running yet.
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
    let serves_this_node = matches!(
        facts.port,
        PortObservation::Gents {
            identity: ServedIdentity::Matches,
            ..
        }
    );
    let serves_another_node = matches!(
        facts.port,
        PortObservation::Gents {
            identity: ServedIdentity::Different { .. },
            ..
        }
    );
    // A crash-looping job is loaded but rarely running, so a broken home
    // stops whatever is loaded.
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
        // The definition records the resolved home, so a symlinked choice is
        // compared through the manager's normalization.
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
            previous_version: None,
        };
        let Some(installed_home) = installed_home else {
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
    // A bundle at an unchanged path is this build's, so only another
    // program's names the version that ran before.
    if let Some(installed) = installed.filter(|_| facts.foreign_program) {
        facts.previous_version = tokio::task::spawn_blocking(move || bundle_version(&installed))
            .await
            .ok()
            .flatten();
    }
    Ok(facts)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
/// Whether a managed update owns `owner`'s program: it selected the program,
/// or its handoff is pending. An update record that can't be read may be a
/// handoff in progress, so it counts as one rather than failing every repair.
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

/// Whether the service's program is compared with this run's: for a move to
/// the chosen folder, which installs anyway, or for this home's silent job.
/// A node that answers or boots never resolves the program.
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

/// A program that cannot be resolved here counts as the same.
async fn program_is_foreign(installed: PathBuf, program: &ServiceProgram) -> bool {
    let program = Arc::clone(program);
    tokio::task::spawn_blocking(move || {
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        program().is_ok_and(|ours| canonical(&installed) != canonical(&ours))
    })
    .await
    .unwrap_or(false)
}

/// `reinstall_installs` says whether a reinstall or switch installs again;
/// the caller decides it once, with the program it resolved for that install.
pub async fn apply(
    action: &ServiceAction,
    manager: &NativeServiceManager,
    reinstall_installs: bool,
) -> Result<(), Blocker> {
    let config = manager.config();
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
                // A home that still needs setup gets its service from setup.
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
            on_manager(&previous, NativeServiceManager::uninstall).await?;
            if install {
                on_manager(config, move |manager| {
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
            on_manager(config, move |manager| {
                manager.uninstall()?;
                if install {
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

/// What a move or reinstall carries into the new definition.
struct Carried {
    definition: Vec<u8>,
    enabled: bool,
    pid: Option<u32>,
}

/// Reads what `old` carries, checks that `target` can install it, and stops
/// the old job. Nothing is removed or disabled yet, so a failure here or a
/// later exit-wait timeout leaves the old service for the next run.
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

/// Stops `manager`'s job for a reinstall, checking the definition first only
/// when it is installed again.
fn stop_for_reinstall<R>(manager: &NativeServiceManager<R>, install: bool) -> Result<Carried>
where
    R: CommandRunner,
{
    stop_for_handover(manager, install.then_some(manager), true)
}

/// Runs launchctl or systemctl work for `config`'s home off the runtime.
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

/// launchd refuses a bootstrap right after a bootout (error 5), so a start
/// that follows a stop is tried once more after a second. A start macOS
/// withholds approval for waits on the user, so it is not retried.
async fn start_after_stop(config: &NativeServiceConfig) -> Result<(), Blocker> {
    match start_job(config).await {
        Err(Blocker::ServiceUpdateFailed { reason }) => {
            tracing::debug!(%reason, "starting after a stop failed; retrying");
            tokio::time::sleep(Duration::from_secs(1)).await;
            start_job(config).await
        }
        started => started,
    }
}

/// Starts `config`'s job; a start macOS withholds until the user approves
/// Gents is reported as that wait.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_service::{CommandOutput, NativeServicePlatform};
    use crate::repair::home::BrokenHome;
    use std::cell::{Cell, RefCell};
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::rc::Rc;

    /// A systemd user manager whose unit is enabled and runs as pid 4321
    /// until it is stopped.
    #[derive(Clone, Default)]
    struct Systemd {
        stopped: Rc<Cell<bool>>,
        calls: Rc<RefCell<Vec<String>>>,
    }

    impl CommandRunner for Systemd {
        fn run(&self, _program: &OsStr, args: &[OsString]) -> Result<CommandOutput> {
            let verb = args[1].to_string_lossy().into_owned();
            self.calls.borrow_mut().push(verb.clone());
            if verb == "stop" {
                self.stopped.set(true);
            }
            let stdout = match verb.as_str() {
                "is-active" if self.stopped.get() => "inactive",
                "is-active" => "active",
                "is-enabled" => "enabled",
                "show" => "4321",
                _ => "",
            };
            Ok(CommandOutput {
                success: stdout != "inactive",
                stdout: stdout.into(),
                stderr: String::new(),
            })
        }
    }

    fn service_config(root: &Path, home: &str) -> NativeServiceConfig {
        let home = root.join(home);
        fs::create_dir_all(&home).unwrap();
        let executable = root.join("gents");
        fs::write(&executable, b"fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        }
        NativeServiceConfig {
            home,
            executable,
            user_home: root.to_path_buf(),
            service_config_dir: root.join("units"),
            search_path: None,
            associated_bundle_id: None,
            stderr_path: None,
        }
    }

    fn linux(config: NativeServiceConfig, runner: &Systemd) -> NativeServiceManager<Systemd> {
        NativeServiceManager::with_runner(config, NativeServicePlatform::Linux, runner.clone())
    }

    /// The old home's installed service, its manager and runner, and the
    /// chosen home's manager.
    fn installed_move(
        root: &Path,
    ) -> (
        NativeServiceManager<Systemd>,
        Systemd,
        NativeServiceManager<Systemd>,
        PathBuf,
    ) {
        let old = service_config(root, "old-home");
        let chosen = service_config(root, "chosen-home");
        let definition = old.definition_path(NativeServicePlatform::Linux);
        linux(old.clone(), &Systemd::default()).install().unwrap();
        let runner = Systemd::default();
        (
            linux(old, &runner),
            runner,
            linux(chosen, &Systemd::default()),
            definition,
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn an_unreadable_update_record_yields_to_the_update() {
        let temp = tempfile::tempdir().unwrap();
        let config = service_config(temp.path(), "home");
        let manager = linux(config.clone(), &Systemd::default());
        manager.install().unwrap();
        assert!(!yields_to_update(&manager).unwrap());
        let update = config
            .home
            .join(gents::store::local::home::SERVICE_UPDATES_DIR_NAME)
            .join("6f1c2b4e-8a3d-4f5e-9b7c-1d2e3f4a5b6c");
        fs::create_dir_all(&update).unwrap();
        fs::write(update.join("receipt.json"), "{").unwrap();
        assert!(yields_to_update(&manager).unwrap());
    }

    #[test]
    fn a_move_whose_wait_times_out_leaves_the_old_service_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let (old, runner, chosen, definition) = installed_move(temp.path());
        let before = fs::read(&definition).unwrap();
        let carried = stop_for_handover(&old, Some(&chosen), false).unwrap();
        assert_eq!(carried.pid, Some(4321));
        assert!(carried.enabled);
        assert_eq!(fs::read(&definition).unwrap(), before);
        let calls = runner.calls.borrow().clone();
        assert!(calls.contains(&"stop".to_string()), "{calls:?}");
        assert!(!calls.contains(&"disable".to_string()), "{calls:?}");
    }

    #[test]
    fn a_move_that_cannot_install_reports_before_touching_the_old_service() {
        let temp = tempfile::tempdir().unwrap();
        let (old, runner, chosen, definition) = installed_move(temp.path());
        let before = fs::read(&definition).unwrap();
        fs::remove_file(temp.path().join("gents")).unwrap();
        assert!(stop_for_handover(&old, Some(&chosen), false).is_err());
        assert_eq!(fs::read(&definition).unwrap(), before);
        let calls = runner.calls.borrow().clone();
        assert!(!calls.contains(&"stop".to_string()), "{calls:?}");
    }

    #[test]
    fn a_reinstall_toward_a_deleted_home_stops_and_installs_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let config = service_config(temp.path(), "home");
        linux(config.clone(), &Systemd::default())
            .install()
            .unwrap();
        fs::remove_dir_all(&config.home).unwrap();
        fs::remove_file(&config.executable).unwrap();
        let runner = Systemd::default();
        let carried = stop_for_reinstall(&linux(config, &runner), false).unwrap();
        assert_eq!(carried.pid, Some(4321));
        let calls = runner.calls.borrow().clone();
        assert!(calls.contains(&"stop".to_string()), "{calls:?}");
    }

    fn chosen() -> PathBuf {
        PathBuf::from("/Users/me/.gents")
    }

    fn serving(identity: ServedIdentity) -> PortObservation {
        PortObservation::Gents {
            identity,
            payload: serde_json::Value::Null,
        }
    }

    fn healthy() -> ServiceFacts {
        ServiceFacts {
            definition_home: Some(chosen()),
            executable_present: true,
            log_current: true,
            job_loaded: true,
            job_running: true,
            job_failed: false,
            port: serving(ServedIdentity::Matches),
            home: HomeState::Complete,
            was_serving: false,
            service_move: ServiceMove::BeforeSetup,
            node_version: Some(NodeVersion::Same),
            foreign_program: false,
            managed_update: false,
            previous_version: None,
        }
    }

    fn version(text: &str) -> semver::Version {
        semver::Version::parse(text).unwrap()
    }

    #[test]
    fn versions_compare_by_semver_and_an_unknown_one_is_older() {
        let rc = version("1.0.0-rc1");
        assert_eq!(
            node_version(Some("0.21.0"), &rc),
            NodeVersion::Older(Some(version("0.21.0")))
        );
        assert_eq!(
            node_version(Some("1.0.0-rc1"), &version("1.0.0")),
            NodeVersion::Older(Some(rc.clone()))
        );
        assert_eq!(
            node_version(Some("1.0.0"), &rc),
            NodeVersion::Newer(version("1.0.0"))
        );
        assert_eq!(node_version(Some("1.0.0-rc1"), &rc), NodeVersion::Same);
        assert_eq!(node_version(None, &rc), NodeVersion::Older(None));
        assert_eq!(node_version(Some("garbage"), &rc), NodeVersion::Older(None));
        assert_eq!(
            served_version(&PortObservation::Gents {
                identity: ServedIdentity::Matches,
                payload: serde_json::json!({ "version": this_version().to_string() }),
            }),
            Some(NodeVersion::Same)
        );
        assert_eq!(
            served_version(&serving(ServedIdentity::Matches)),
            Some(NodeVersion::Older(None)),
            "a status without a version predates the field"
        );
        assert_eq!(served_version(&PortObservation::NotListening), None);
    }

    #[test]
    fn an_older_node_its_service_runs_moves_to_this_program() {
        // v0.21: the status names the node, so it matches.
        let v021 = ServiceFacts {
            node_version: Some(NodeVersion::Older(Some(version("0.21.0")))),
            ..healthy()
        };
        assert_eq!(
            plan_service(&v021, &chosen()),
            vec![ServiceAction::SwitchProgram {
                from: Some(version("0.21.0")),
                answered: true,
                start_after: true
            }]
        );
        // v0.19: the status names no node.
        let v019 = ServiceFacts {
            port: serving(ServedIdentity::Different {
                node_did: None,
                home: None,
            }),
            node_version: Some(NodeVersion::Older(Some(version("0.19.0")))),
            log_current: false,
            ..healthy()
        };
        assert_eq!(
            plan_service(&v019, &chosen()),
            vec![ServiceAction::SwitchProgram {
                from: Some(version("0.19.0")),
                answered: true,
                start_after: true
            }]
        );
        let hand_started = ServiceFacts {
            job_running: false,
            job_loaded: false,
            ..v021
        };
        assert_eq!(plan_service(&hand_started, &chosen()), vec![]);
    }

    #[test]
    fn a_newer_node_blocks_and_others_do_not() {
        let newer = ServiceFacts {
            node_version: Some(NodeVersion::Newer(version("1.1.0"))),
            ..healthy()
        };
        assert_eq!(
            newer_node(&newer),
            Some(Blocker::NewerNode {
                running: "1.1.0".into(),
                ours: this_version().to_string()
            })
        );
        for facts in [
            healthy(),
            ServiceFacts {
                node_version: Some(NodeVersion::Older(None)),
                ..healthy()
            },
            ServiceFacts {
                node_version: None,
                port: PortObservation::NotListening,
                ..healthy()
            },
        ] {
            assert_eq!(newer_node(&facts), None, "{facts:?}");
        }
    }

    #[test]
    fn the_same_version_under_another_program_is_left_alone() {
        let facts = ServiceFacts {
            foreign_program: false,
            ..healthy()
        };
        assert_eq!(plan_service(&facts, &chosen()), vec![]);
    }

    /// The bare home an old crash-looping job recreates: no settings.
    fn crash_looping_old_program(home: HomeState) -> ServiceFacts {
        ServiceFacts {
            job_running: false,
            port: PortObservation::NotListening,
            node_version: None,
            foreign_program: true,
            home,
            ..healthy()
        }
    }

    #[test]
    fn a_silent_service_on_another_program_moves_to_this_one() {
        assert_eq!(
            plan_service(
                &crash_looping_old_program(HomeState::Broken(BrokenHome::InitConfigMissing)),
                &chosen()
            ),
            vec![ServiceAction::SwitchProgram {
                from: None,
                answered: false,
                start_after: false
            }]
        );
        assert_eq!(
            plan_service(&crash_looping_old_program(HomeState::Complete), &chosen()),
            vec![ServiceAction::SwitchProgram {
                from: None,
                answered: false,
                start_after: true
            }]
        );
        let stopped = ServiceFacts {
            job_loaded: false,
            ..crash_looping_old_program(HomeState::Complete)
        };
        assert_eq!(
            plan_service(&stopped, &chosen()),
            vec![ServiceAction::SwitchProgram {
                from: None,
                answered: false,
                start_after: false
            }]
        );
        let failed = ServiceFacts {
            job_failed: true,
            ..stopped
        };
        assert_eq!(
            plan_service(&failed, &chosen()),
            vec![ServiceAction::SwitchProgram {
                from: None,
                answered: false,
                start_after: true
            }]
        );
    }

    #[test]
    fn a_silent_switch_names_the_version_its_programs_bundle_reports() {
        let rc1 = ServiceFacts {
            previous_version: Some(version("1.0.0-rc1")),
            ..crash_looping_old_program(HomeState::Complete)
        };
        assert_eq!(
            plan_service(&rc1, &chosen()),
            vec![ServiceAction::SwitchProgram {
                from: Some(version("1.0.0-rc1")),
                answered: false,
                start_after: true
            }]
        );
    }

    #[test]
    fn only_an_older_version_in_the_programs_app_bundle_is_read() {
        let temp = tempfile::tempdir().unwrap();
        let bundled = |name: &str, short_version: Option<&str>| {
            let contents = temp.path().join(name).join("Gents.app/Contents");
            fs::create_dir_all(contents.join("MacOS")).unwrap();
            let mut info = plist::Dictionary::new();
            if let Some(short_version) = short_version {
                info.insert("CFBundleShortVersionString".into(), short_version.into());
            }
            plist::Value::Dictionary(info)
                .to_file_xml(contents.join("Info.plist"))
                .unwrap();
            contents.join("MacOS/gents")
        };
        assert_eq!(
            bundle_version(&bundled("rc1", Some("0.21.0"))),
            Some(version("0.21.0"))
        );
        assert_eq!(bundle_version(&bundled("newer", Some("99.0.0"))), None);
        assert_eq!(
            bundle_version(&bundled("ours", Some(&this_version().to_string()))),
            None
        );
        assert_eq!(bundle_version(&bundled("garbage", Some("rc"))), None);
        assert_eq!(bundle_version(&bundled("unnamed", None)), None);
        assert_eq!(bundle_version(&temp.path().join("bin/gents")), None);
    }

    #[test]
    fn a_booting_node_on_another_program_is_left_alone() {
        let booting = ServiceFacts {
            port: PortObservation::NotListening,
            node_version: None,
            foreign_program: true,
            ..healthy()
        };
        assert_eq!(plan_service(&booting, &chosen()), vec![]);
        assert!(!compares_program(&booting, &chosen()));
    }

    #[test]
    fn a_managed_update_keeps_its_program() {
        let older = ServiceFacts {
            node_version: Some(NodeVersion::Older(Some(version("0.21.0")))),
            managed_update: true,
            ..healthy()
        };
        assert_eq!(plan_service(&older, &chosen()), vec![]);
        let silent = ServiceFacts {
            managed_update: true,
            ..crash_looping_old_program(HomeState::Complete)
        };
        assert!(!compares_program(&silent, &chosen()));
        let elsewhere = ServiceFacts {
            definition_home: Some(PathBuf::from("/Users/me/other")),
            ..silent
        };
        assert!(!compares_program(&elsewhere, &chosen()));
    }

    #[test]
    fn a_move_installs_this_program_when_the_service_runs_another() {
        let other = PathBuf::from("/Users/me/other");
        let facts = ServiceFacts {
            definition_home: Some(other.clone()),
            foreign_program: true,
            ..healthy()
        };
        assert!(compares_program(&facts, &chosen()));
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::MoveFrom {
                from: other,
                start_after: true,
                replace_program: true
            }]
        );
    }

    #[test]
    fn only_a_move_or_a_silent_job_has_its_program_compared() {
        assert!(compares_program(
            &crash_looping_old_program(HomeState::Complete),
            &chosen()
        ));
        assert!(!compares_program(&healthy(), &chosen()));
        let gone = ServiceFacts {
            executable_present: false,
            ..crash_looping_old_program(HomeState::Complete)
        };
        assert!(!compares_program(&gone, &chosen()));
        let undefined = ServiceFacts {
            definition_home: None,
            ..crash_looping_old_program(HomeState::Complete)
        };
        assert!(!compares_program(&undefined, &chosen()));
    }

    fn resolves_to(path: &'static str) -> ServiceProgram {
        Arc::new(move || Ok(PathBuf::from(path)))
    }

    #[tokio::test]
    async fn another_path_is_another_program_and_an_unresolvable_one_is_not() {
        let old = PathBuf::from("/Applications/Gents 0.19.app/gents");
        assert!(program_is_foreign(old.clone(), &resolves_to("/opt/gents/bin/gents")).await);
        assert!(
            !program_is_foreign(
                old.clone(),
                &resolves_to("/Applications/Gents 0.19.app/gents")
            )
            .await
        );
        let unresolvable: ServiceProgram = Arc::new(|| {
            Err(Blocker::ServiceUpdateFailed {
                reason: "Gents is running from a disk image".into(),
            })
        });
        assert!(!program_is_foreign(old, &unresolvable).await);
    }

    #[test]
    fn the_cli_moves_the_service_only_once_the_folder_is_set_up() {
        let other = PathBuf::from("/Users/me/other");
        let facts = ServiceFacts {
            definition_home: Some(other.clone()),
            service_move: ServiceMove::AfterSetup,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::MoveFrom {
                from: other,
                start_after: true,
                replace_program: false
            }]
        );
        for home in [
            HomeState::Absent,
            HomeState::Broken(BrokenHome::InitConfigMissing),
        ] {
            let needs_setup = ServiceFacts {
                home,
                ..facts.clone()
            };
            assert_eq!(plan_service(&needs_setup, &chosen()), vec![]);
        }
    }

    #[test]
    fn a_node_its_supervisor_gave_up_on_is_restarted() {
        let failed = ServiceFacts {
            job_loaded: false,
            job_running: false,
            job_failed: true,
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(
            plan_service(&failed, &chosen()),
            vec![ServiceAction::Restart]
        );
        let old_log = ServiceFacts {
            log_current: false,
            ..failed.clone()
        };
        assert_eq!(
            plan_service(&old_log, &chosen()),
            vec![
                ServiceAction::RefreshLog { restart: false },
                ServiceAction::Restart
            ]
        );
        let broken = ServiceFacts {
            home: HomeState::Broken(BrokenHome::InitConfigMissing),
            ..failed.clone()
        };
        assert_eq!(plan_service(&broken, &chosen()), vec![]);
        let stopped = ServiceFacts {
            job_failed: false,
            ..failed
        };
        assert_eq!(plan_service(&stopped, &chosen()), vec![]);
    }

    #[test]
    fn no_definition_needs_nothing() {
        let facts = ServiceFacts {
            definition_home: None,
            log_current: false,
            ..healthy()
        };
        assert_eq!(plan_service(&facts, &chosen()), vec![]);
    }

    #[test]
    fn a_definition_for_another_folder_is_moved_and_restarted_when_it_was_running() {
        let other = PathBuf::from("/Users/me/other");
        let facts = ServiceFacts {
            definition_home: Some(other.clone()),
            executable_present: false,
            log_current: false,
            port: serving(ServedIdentity::Different {
                node_did: None,
                home: None,
            }),
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::MoveFrom {
                from: other.clone(),
                start_after: true,
                replace_program: false
            }]
        );
        let needs_setup = ServiceFacts {
            home: HomeState::Absent,
            ..facts.clone()
        };
        assert_eq!(
            plan_service(&needs_setup, &chosen()),
            vec![ServiceAction::MoveFrom {
                from: other.clone(),
                start_after: false,
                replace_program: false
            }]
        );
        let unloaded = ServiceFacts {
            job_loaded: false,
            job_running: false,
            ..facts
        };
        assert_eq!(
            plan_service(&unloaded, &chosen()),
            vec![ServiceAction::MoveFrom {
                from: other,
                start_after: false,
                replace_program: false
            }]
        );
    }

    #[test]
    fn a_move_back_to_the_folder_that_was_serving_starts_its_node() {
        // A refused start leaves a systemd unit failed, so its job reads unloaded.
        let other = PathBuf::from("/srv/work");
        let facts = ServiceFacts {
            definition_home: Some(other.clone()),
            job_loaded: false,
            job_running: false,
            port: PortObservation::NotListening,
            was_serving: true,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::MoveFrom {
                from: other.clone(),
                start_after: true,
                replace_program: false
            }]
        );
        let needs_setup = ServiceFacts {
            home: HomeState::Absent,
            ..facts
        };
        assert_eq!(
            plan_service(&needs_setup, &chosen()),
            vec![ServiceAction::MoveFrom {
                from: other,
                start_after: false,
                replace_program: false
            }]
        );
    }

    #[test]
    fn a_missing_program_is_reinstalled_and_restarted_if_it_was_loaded() {
        let facts = ServiceFacts {
            executable_present: false,
            job_running: false,
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::Reinstall { start_after: true }]
        );
        let unloaded = ServiceFacts {
            job_loaded: false,
            ..facts
        };
        assert_eq!(
            plan_service(&unloaded, &chosen()),
            vec![ServiceAction::Reinstall { start_after: false }]
        );
    }

    #[test]
    fn a_running_node_for_a_deleted_folder_is_stopped() {
        let facts = ServiceFacts {
            home: HomeState::Absent,
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::StopStale]
        );
        let broken = ServiceFacts {
            home: HomeState::Broken(BrokenHome::InitConfigMissing),
            ..facts
        };
        assert_eq!(
            plan_service(&broken, &chosen()),
            vec![ServiceAction::StopStale]
        );
    }

    #[test]
    fn a_crash_looping_node_for_a_broken_home_is_stopped() {
        let facts = ServiceFacts {
            job_running: false,
            home: HomeState::Broken(BrokenHome::SetupStoppedMidway),
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::StopStale]
        );
    }

    #[test]
    fn a_reinstall_on_a_broken_home_does_not_start() {
        let facts = ServiceFacts {
            executable_present: false,
            job_running: false,
            home: HomeState::Broken(BrokenHome::InitConfigMissing),
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::Reinstall { start_after: false }]
        );
    }

    #[test]
    fn a_refresh_on_a_broken_home_does_not_restart() {
        let facts = ServiceFacts {
            log_current: false,
            job_loaded: false,
            job_running: false,
            home: HomeState::Broken(BrokenHome::InitConfigUnreadable),
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::RefreshLog { restart: false }]
        );
        let loaded = ServiceFacts {
            job_loaded: true,
            ..facts
        };
        assert_eq!(
            plan_service(&loaded, &chosen()),
            vec![
                ServiceAction::StopStale,
                ServiceAction::RefreshLog { restart: false }
            ]
        );
    }

    #[test]
    fn a_running_node_with_another_id_is_stopped() {
        let facts = ServiceFacts {
            port: serving(ServedIdentity::Different {
                node_did: Some("did:key:zother".into()),
                home: None,
            }),
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::StopStale]
        );
    }

    #[test]
    fn a_matching_running_node_is_left_alone() {
        assert_eq!(plan_service(&healthy(), &chosen()), vec![]);
    }

    #[test]
    fn a_stopped_node_is_left_stopped() {
        let facts = ServiceFacts {
            job_loaded: false,
            job_running: false,
            home: HomeState::Absent,
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(plan_service(&facts, &chosen()), vec![]);
    }

    #[test]
    fn an_old_log_path_is_refreshed_without_restarting_a_healthy_node() {
        let facts = ServiceFacts {
            log_current: false,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::RefreshLog { restart: false }]
        );
        let stopped = ServiceFacts {
            job_loaded: false,
            job_running: false,
            port: PortObservation::NotListening,
            ..facts
        };
        assert_eq!(
            plan_service(&stopped, &chosen()),
            vec![ServiceAction::RefreshLog { restart: false }]
        );
    }

    #[test]
    fn an_old_log_path_restarts_a_failing_node() {
        let facts = ServiceFacts {
            log_current: false,
            job_running: false,
            port: PortObservation::NotListening,
            ..healthy()
        };
        assert_eq!(
            plan_service(&facts, &chosen()),
            vec![ServiceAction::RefreshLog { restart: true }]
        );
        let stale = ServiceFacts {
            job_running: true,
            home: HomeState::Absent,
            ..facts
        };
        assert_eq!(
            plan_service(&stale, &chosen()),
            vec![
                ServiceAction::StopStale,
                ServiceAction::RefreshLog { restart: false }
            ]
        );
    }
}
