use std::sync::{Arc, Mutex};

use gents_protocol::serve_lifecycle::replacement::{Action, Phase, RetirementAuthority, State};
use tokio::sync::watch;

/// Captured before the serving process publishes readiness. The digest fences
/// that executable; it is neither node identity nor evidence of build ordering.
pub async fn executable_build_id() -> anyhow::Result<String> {
    use anyhow::Context as _;
    static BUILD: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    let build = BUILD
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                use sha2::{Digest, Sha256};
                use std::io::Read as _;
                let mut file = std::fs::File::open(std::env::current_exe()?)?;
                let mut digest = Sha256::new();
                let mut buffer = [0u8; 64 * 1024];
                loop {
                    let length = file.read(&mut buffer)?;
                    if length == 0 {
                        break;
                    }
                    digest.update(&buffer[..length]);
                }
                Ok::<_, anyhow::Error>(format!("{:x}", digest.finalize()))
            })
            .await
            .context("joining runtime executable observation")?
        })
        .await?;
    Ok(build.clone())
}

pub fn verify_control_receipt(
    command: &gents_protocol::serve_lifecycle::control::RuntimeControlCommand,
    receipt: &gents_protocol::serve_lifecycle::control::RuntimeControlReceipt,
) -> anyhow::Result<()> {
    receipt.validate_for(command, chrono::Utc::now())?;
    anyhow::ensure!(
        crate::identity::verify_did_signature(
            &command.node_did,
            &receipt.signing_payload()?,
            &receipt.signature,
        )?,
        "runtime control receipt signature is invalid"
    );
    Ok(())
}

pub async fn sign_control_receipt(
    identity: &dyn crate::identity::NodeIdentity,
    receipt: &mut gents_protocol::serve_lifecycle::control::RuntimeControlReceipt,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        receipt.node_did == identity.did(),
        "runtime receipt belongs to another node"
    );
    receipt.signature = identity.sign(&receipt.signing_payload()?).await?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RetirementError {
    #[error("runtime operator authorization is required")]
    Unauthorized,
    #[error("the runtime build changed; inspect the running runtime again")]
    BuildChanged,
    #[error("the runtime still owns active work; finish or stop that work before updating")]
    Busy,
    #[error("the runtime is stopping; reconnect after it has released its store")]
    Stopping,
    #[error("runtime replacement admission is unavailable")]
    Unavailable,
}

struct Inner {
    state: Mutex<State>,
    phase: watch::Sender<Phase>,
}

/// One process-local fence shared by request claims and detached work. A work
/// guard is acquired before dispatch and moves with the worker, so dropping
/// its caller cannot make an in-flight execution appear idle.
#[derive(Clone)]
pub struct RuntimeServiceAdmission(Arc<Inner>);

impl Default for RuntimeServiceAdmission {
    fn default() -> Self {
        let (phase, _) = watch::channel(Phase::Accepting);
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                phase: Phase::Accepting,
                executions: 0,
                native_outstanding: 0,
            }),
            phase,
        }))
    }
}

impl RuntimeServiceAdmission {
    pub fn is_retiring(&self) -> Result<bool, RetirementError> {
        let state = self
            .0
            .state
            .lock()
            .map_err(|_| RetirementError::Unavailable)?;
        Ok(state.phase != Phase::Accepting)
    }
    pub(crate) fn admit(&self) -> Result<RuntimeServiceWork, RetirementError> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| RetirementError::Unavailable)?;
        *state = state.step(Action::Admit).ok_or_else(|| {
            if state.phase == Phase::Accepting {
                RetirementError::Unavailable
            } else {
                RetirementError::Stopping
            }
        })?;
        Ok(RuntimeServiceWork(self.clone()))
    }

    pub fn retire(&self, authority: RetirementAuthority) -> Result<(), RetirementError> {
        if !authority.operator_authenticated {
            return Err(RetirementError::Unauthorized);
        }
        if !authority.observed_build_matches {
            return Err(RetirementError::BuildChanged);
        }
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| RetirementError::Unavailable)?;
        *state = state.step(Action::Retire(authority)).ok_or_else(|| {
            if state.phase != Phase::Accepting {
                RetirementError::Stopping
            } else {
                RetirementError::Busy
            }
        })?;
        self.0.phase.send_replace(state.phase);
        Ok(())
    }

    pub async fn retirement_requested(&self) {
        let mut phase = self.0.phase.subscribe();
        while *phase.borrow_and_update() == Phase::Accepting {
            if phase.changed().await.is_err() {
                return;
            }
        }
    }
}

pub(crate) struct RuntimeServiceWork(RuntimeServiceAdmission);

impl Drop for RuntimeServiceWork {
    fn drop(&mut self) {
        let Ok(mut state) = self.0 .0.state.lock() else {
            tracing::error!("runtime replacement admission was poisoned during work release");
            return;
        };
        let Some(next) = state.step(Action::Finish) else {
            tracing::error!("runtime replacement work guard lost its admission");
            return;
        };
        *state = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authorized() -> RetirementAuthority {
        RetirementAuthority {
            operator_authenticated: true,
            observed_build_matches: true,
        }
    }

    #[tokio::test]
    async fn a_detached_worker_keeps_retirement_busy_until_its_owned_work_finishes() {
        let admission = RuntimeServiceAdmission::default();
        let work = admission.admit().unwrap();
        let (release, wait) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let _work = work;
            wait.await.unwrap();
        });
        assert_eq!(admission.retire(authorized()), Err(RetirementError::Busy));
        release.send(()).unwrap();
        worker.await.unwrap();
        admission.retire(authorized()).unwrap();
        assert!(matches!(admission.admit(), Err(RetirementError::Stopping)));
        admission.retirement_requested().await;
    }

    #[tokio::test]
    async fn a_refused_retirement_keeps_admission_open_and_does_not_signal_shutdown() {
        let admission = RuntimeServiceAdmission::default();
        for authority in [
            RetirementAuthority {
                operator_authenticated: false,
                observed_build_matches: true,
            },
            RetirementAuthority {
                operator_authenticated: true,
                observed_build_matches: false,
            },
        ] {
            assert!(admission.retire(authority).is_err());
            let work = admission.admit().unwrap();
            assert_eq!(*admission.0.phase.borrow(), Phase::Accepting);
            drop(work);
        }
        admission.retire(authorized()).unwrap();
        assert_eq!(
            admission.retire(authorized()),
            Err(RetirementError::Stopping)
        );
    }
}
