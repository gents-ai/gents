use std::time::Duration;

use tokio::time::Instant;

/// Lean `RuntimeReconcile.SurfacePublication.epochDeadline`.
pub(super) fn epoch_deadline(opened: Option<Instant>, now: Instant, budget: Duration) -> Instant {
    opened.unwrap_or(now + budget)
}

/// Lean `RuntimeReconcile.SurfacePublication.Disposition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Disposition {
    Publish,
    KeepPrevious,
    PublishDemotingBusy,
}

/// Lean `RuntimeReconcile.SurfacePublication.reconcile`.
pub(super) fn reconcile(busy: bool, now: Instant, deadline: Instant) -> Disposition {
    if !busy {
        Disposition::Publish
    } else if now < deadline {
        Disposition::KeepPrevious
    } else {
        Disposition::PublishDemotingBusy
    }
}

/// Lean `RuntimeReconcile.SurfacePublication.sendProposal`.
pub(super) fn send_proposal(changed: bool, recovery_pending: bool) -> bool {
    changed || recovery_pending
}

/// Lean `RuntimeReconcile.SurfacePublication.retryAfterFailure`.
pub(super) fn retry_after_failure(settling: bool, recovery_pending: bool) -> bool {
    settling || recovery_pending
}

/// Lean `RuntimeReconcile.SurfacePublication.StartupCause`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StartupCause {
    Degraded,
    PublicationBusy,
    Invalid,
}

/// Lean `RuntimeReconcile.SurfacePublication.startupCause`.
pub(super) fn startup_cause(
    degraded_reason: bool,
    surface_unavailable: bool,
    publication_busy: bool,
) -> StartupCause {
    if degraded_reason {
        StartupCause::Degraded
    } else if surface_unavailable && publication_busy {
        StartupCause::PublicationBusy
    } else {
        StartupCause::Invalid
    }
}

/// Lean `RuntimeReconcile.SurfacePublication.startupProceeds`.
pub(super) fn startup_proceeds(runnable: usize, causes: &[StartupCause]) -> bool {
    runnable > 0
        || (!causes.is_empty() && causes.iter().all(|cause| *cause != StartupCause::Invalid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_publication_epoch_replays_the_lean_owner() {
        let base = Instant::now();
        let at = |millis: u64| base + Duration::from_millis(millis);
        gents_lean_vocab::families::surface_publication_epoch_deadline::replay(|i| {
            let deadline =
                epoch_deadline(i.opened.map(at), at(i.now), Duration::from_millis(i.budget));
            u64::try_from((deadline - base).as_millis()).unwrap()
        });
        gents_lean_vocab::families::surface_publication_reconcile::replay::<Disposition>(|i| {
            reconcile(i.busy, at(i.now), at(i.deadline))
        });
    }

    #[test]
    fn recovery_and_startup_decisions_replay_the_lean_owner() {
        gents_lean_vocab::families::runtime_reconcile_surface_publication_send_proposal::replay(
            |i| send_proposal(i.changed, i.recovery_pending),
        );
        gents_lean_vocab::families::runtime_reconcile_surface_publication_retry_after_failure::replay(|i| {
            retry_after_failure(i.settling, i.recovery_pending)
        });
        gents_lean_vocab::families::runtime_reconcile_surface_publication_startup_cause::replay::<
            StartupCause,
        >(|i| startup_cause(i.degraded_reason, i.surface_unavailable, i.publication_busy));
        gents_lean_vocab::families::surface_publication_startup_proceeds::replay::<StartupCause>(
            |i| startup_proceeds(usize::try_from(i.runnable).unwrap(), &i.causes),
        );
    }
}
