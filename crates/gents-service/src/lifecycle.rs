//! The native service lifecycle every client calls. A direct action is
//! admitted here before it reaches the service manager; a managed update is
//! `service_update`'s operation and has its own gates.
use gents_protocol::operations::service::{
    decide, ServiceLifecycleAction, ServiceLifecycleCallerObservation, ServiceLifecycleDecision,
    ServiceLifecycleRefusal,
};

use crate::args::ServiceAction;
use crate::native_service::{CommandRunner, NativeServiceManager, NativeServiceStatus};
use crate::network_settings::NetworkSettings;

mod ancestry;

/// A refusal, or the service manager's own failure unchanged: callers that
/// classify a native failure (a pending background approval) still find it.
#[derive(Debug)]
pub enum ServiceLifecycleError {
    Refused(ServiceLifecycleRefusal),
    Native(anyhow::Error),
}

impl From<anyhow::Error> for ServiceLifecycleError {
    fn from(error: anyhow::Error) -> Self {
        Self::Native(error)
    }
}

impl From<ServiceLifecycleError> for anyhow::Error {
    fn from(error: ServiceLifecycleError) -> Self {
        match error {
            ServiceLifecycleError::Refused(refusal) => refusal.into(),
            ServiceLifecycleError::Native(error) => error,
        }
    }
}

pub enum ServiceLifecycleReceipt {
    Done,
    Installed(NetworkSettings),
    Status {
        service: NativeServiceStatus,
        network: NetworkSettings,
    },
    /// `removed` is false when no definition was installed at `definition`.
    Uninstalled {
        definition: std::path::PathBuf,
        removed: bool,
    },
}

impl ServiceAction {
    pub fn lifecycle(&self) -> ServiceLifecycleAction {
        match self {
            Self::Install { .. } => ServiceLifecycleAction::Install,
            Self::Start { .. } => ServiceLifecycleAction::Start,
            Self::Stop { .. } => ServiceLifecycleAction::Stop,
            Self::Restart => ServiceLifecycleAction::Restart,
            Self::Enable => ServiceLifecycleAction::Enable,
            Self::Disable => ServiceLifecycleAction::Disable,
            Self::Status => ServiceLifecycleAction::Status,
            Self::Uninstall => ServiceLifecycleAction::Uninstall,
        }
    }
}

/// What this process observes about the service manager's running job. A
/// service manager that cannot be asked fails the action, as it would fail
/// the action itself.
fn observe_caller<R>(
    manager: &NativeServiceManager<R>,
) -> anyhow::Result<ServiceLifecycleCallerObservation>
where
    R: CommandRunner,
{
    Ok(match manager.job_pid()? {
        None => ServiceLifecycleCallerObservation::NoRuntime,
        Some(job) => match ancestry::is_ancestor(job) {
            Some(true) => ServiceLifecycleCallerObservation::Ancestor,
            Some(false) => ServiceLifecycleCallerObservation::NotAncestor,
            None => ServiceLifecycleCallerObservation::Unobservable,
        },
    })
}

/// Whether `action` on this manager's home may run now, for a caller that
/// must know before it prepares the action.
pub fn admit<R>(
    manager: &NativeServiceManager<R>,
    action: ServiceLifecycleAction,
) -> Result<(), ServiceLifecycleError>
where
    R: CommandRunner,
{
    let home = manager.home();
    let caller_runs_on_target =
        action.disrupts() && observe_caller(manager)?.caller_runs_on_target();
    let pending = if action.contends_with_update() {
        manager.pending_update()?
    } else {
        None
    };
    let named_home = || home.display().to_string();
    match decide(action, caller_runs_on_target, pending.is_some()) {
        ServiceLifecycleDecision::Admitted => Ok(()),
        ServiceLifecycleDecision::WouldDisruptCaller => Err(ServiceLifecycleError::Refused(
            ServiceLifecycleRefusal::WouldDisruptCaller {
                action,
                home: named_home(),
            },
        )),
        ServiceLifecycleDecision::UpdateInProgress => Err(ServiceLifecycleError::Refused(
            ServiceLifecycleRefusal::UpdateInProgress {
                action,
                home: named_home(),
                operation_id: pending.map(|id| id.to_string()).unwrap_or_default(),
            },
        )),
    }
}

/// Refuses the managed update's helper when it runs under the job it
/// restarts.
pub fn refuse_update_from_its_runtime<R>(
    manager: &NativeServiceManager<R>,
) -> Result<(), ServiceLifecycleError>
where
    R: CommandRunner,
{
    if observe_caller(manager)?.caller_runs_on_target() {
        return Err(ServiceLifecycleError::Refused(
            ServiceLifecycleRefusal::WouldDisruptCaller {
                action: ServiceLifecycleAction::Restart,
                home: manager.home().display().to_string(),
            },
        ));
    }
    Ok(())
}

/// Admits `action` and runs it on the service manager. Blocking: it runs
/// the platform's service commands.
pub fn perform<R>(
    manager: &NativeServiceManager<R>,
    action: &ServiceAction,
) -> Result<ServiceLifecycleReceipt, ServiceLifecycleError>
where
    R: CommandRunner,
{
    admit(manager, action.lifecycle())?;
    match action {
        ServiceAction::Install {
            network,
            environment_file,
        } => {
            let environment = environment_file
                .as_deref()
                .map(crate::native_service::read_environment_file)
                .transpose()?;
            let settings = crate::network_settings::install_service(
                manager,
                manager.home(),
                network,
                environment.as_ref(),
            )?;
            return Ok(ServiceLifecycleReceipt::Installed(settings));
        }
        ServiceAction::Start { enable } => manager.start(*enable)?,
        ServiceAction::Stop { keep_enabled } => manager.stop(!keep_enabled)?,
        ServiceAction::Restart => manager.restart()?,
        ServiceAction::Enable => manager.set_enabled(true)?,
        ServiceAction::Disable => manager.set_enabled(false)?,
        ServiceAction::Status => {
            return Ok(ServiceLifecycleReceipt::Status {
                service: manager.status()?,
                network: NetworkSettings::read(manager.home())?,
            })
        }
        ServiceAction::Uninstall => {
            let definition = manager.definition_path();
            let removed = definition.exists();
            manager.uninstall()?;
            return Ok(ServiceLifecycleReceipt::Uninstalled {
                definition,
                removed,
            });
        }
    }
    Ok(ServiceLifecycleReceipt::Done)
}

#[cfg(test)]
mod tests {
    use gents_lean_vocab::families;
    use gents_protocol::operations::service::{
        decide, ServiceLifecycleAction, ServiceLifecycleCallerObservation,
    };

    #[test]
    fn lifecycle_admission_matches_lean() {
        families::service_lifecycle_action_disrupts::replay(|i| {
            ServiceLifecycleAction::disrupts(i.action)
        });
        families::service_lifecycle_action_contends_with_update::replay(|i| {
            ServiceLifecycleAction::contends_with_update(i.action)
        });
        families::service_lifecycle_caller_observation_caller_runs_on_target::replay(|i| {
            ServiceLifecycleCallerObservation::caller_runs_on_target(i.caller_observation)
        });
        families::service_lifecycle_admission::replay(|i| {
            decide(i.action, i.caller_runs_on_target, i.update_pending)
        });
    }
}
