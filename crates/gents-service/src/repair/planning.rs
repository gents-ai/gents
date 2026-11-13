use gents_protocol::operations::install_repair::{
    HomeState, RefusalAction, RefusalObservation, RepairAnswer, RepairObservation, RepairOperation,
    RepairPlan, RepairPlanBlocker, RepairStep, RepairVersion,
};

pub fn needs_archive(home: HomeState) -> bool {
    matches!(
        home,
        HomeState::Interrupted
            | HomeState::UnreadableSettings
            | HomeState::MissingSettings
            | HomeState::MissingStoreKey
            | HomeState::CorruptStore
    )
}

fn answered(observed: &RepairObservation) -> RepairAnswer {
    if observed.interactive {
        observed.answer
    } else {
        RepairAnswer::None
    }
}

pub fn sets_aside(observed: &RepairObservation) -> bool {
    (needs_archive(observed.home) || observed.home == HomeState::UnfinishedSetup)
        && answered(observed) == RepairAnswer::SetAside
}

fn continues_setup(observed: &RepairObservation) -> bool {
    observed.home == HomeState::UnfinishedSetup && answered(observed) == RepairAnswer::ContinueSetup
}

fn usable(observed: &RepairObservation, archive: bool) -> bool {
    observed.home == HomeState::Healthy || (observed.home == HomeState::UnfinishedSetup && !archive)
}

pub fn keeps_served_home(observed: &RepairObservation) -> bool {
    observed.service_present
        && !observed.service_same_home
        && observed.served_home_present
        && observed.operation != RepairOperation::Repoint
}

fn service_steps(observed: &RepairObservation, archive: bool, setup: bool) -> Vec<RepairStep> {
    let update = !observed.managed_update
        && (observed.version == RepairVersion::Older
            || !observed.service_program_present
            || !observed.service_same_home);
    let stop = observed.service_running && (setup || update);
    let drain = stop && usable(observed, archive);
    let restart = update && observed.service_running && !setup;
    let mut steps = Vec::new();
    if drain {
        steps.push(if observed.drain_supported {
            RepairStep::Drain
        } else {
            RepairStep::Backup
        });
    }
    if stop {
        steps.push(RepairStep::StopOldNode);
    }
    if update && usable(observed, archive) {
        steps.push(RepairStep::ColdBackup);
    }
    if archive {
        steps.push(RepairStep::SetAsideFolder);
    }
    if update {
        steps.push(RepairStep::UpdateService);
    }
    if restart {
        steps.extend([RepairStep::StartNode, RepairStep::WaitForNode]);
    }
    steps
}

fn blocker_of(observed: &RepairObservation) -> Option<RepairPlanBlocker> {
    if observed.version == RepairVersion::Newer || observed.home == HomeState::NewerStore {
        Some(RepairPlanBlocker::NewerVersion)
    } else if observed.home == HomeState::OlderStore {
        Some(RepairPlanBlocker::OlderStore)
    } else if observed.home == HomeState::InsecureKey {
        Some(RepairPlanBlocker::InsecureKey)
    } else if !observed.readable {
        Some(RepairPlanBlocker::UnreadableHome)
    } else if observed.keychain_locked {
        Some(RepairPlanBlocker::Keychain)
    } else if observed.foreign_process {
        Some(RepairPlanBlocker::ForeignProcess)
    } else if needs_archive(observed.home) && !sets_aside(observed) {
        Some(if observed.interactive {
            RepairPlanBlocker::Ask
        } else {
            RepairPlanBlocker::Unattended
        })
    } else if observed.home == HomeState::UnfinishedSetup
        && answered(observed) == RepairAnswer::None
        && observed.interactive
    {
        Some(RepairPlanBlocker::UnfinishedSetup)
    } else {
        None
    }
}

pub fn plan(observed: &RepairObservation) -> RepairPlan {
    let blocked = |blocker| RepairPlan {
        steps: Vec::new(),
        blocker,
        needs_setup: false,
    };
    if let Some(blocker) = blocker_of(observed) {
        return blocked(blocker);
    }
    let archive = sets_aside(observed);
    let setup = archive || continues_setup(observed) || observed.home == HomeState::Absent;
    let steps = if observed.service_present && !keeps_served_home(observed) {
        service_steps(observed, archive, setup)
    } else if archive {
        vec![RepairStep::SetAsideFolder]
    } else {
        Vec::new()
    };
    if observed.busy && (setup || !steps.is_empty()) {
        return blocked(RepairPlanBlocker::Busy);
    }
    RepairPlan {
        steps,
        blocker: RepairPlanBlocker::None,
        needs_setup: setup,
    }
}

pub fn refusal_action(observed: &RefusalObservation) -> RefusalAction {
    if !observed.fresh {
        RefusalAction::Ignore
    } else if observed.newer_store || observed.older_store {
        RefusalAction::Block
    } else if observed.missing_key {
        RefusalAction::Ask
    } else {
        RefusalAction::Block
    }
}
