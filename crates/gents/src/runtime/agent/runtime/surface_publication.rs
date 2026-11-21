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
}
