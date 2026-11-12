use gents_protocol::operations::install_repair::{
    HomeState, RefusalAction, RefusalObservation, RepairObservation, RepairPlan, RepairPlanBlocker,
    RepairStep, RepairVersion,
};

pub fn needs_archive(home: HomeState) -> bool {
    matches!(
        home,
        HomeState::Interrupted
            | HomeState::UnreadableSettings
            | HomeState::MissingSettings
            | HomeState::MissingStoreKey
            | HomeState::OlderStore
    )
}

pub fn plan(observed: &RepairObservation) -> RepairPlan {
    let blocked = |blocker| RepairPlan {
        steps: Vec::new(),
        blocker,
        needs_setup: false,
    };
    if observed.version == RepairVersion::Newer || observed.home == HomeState::NewerStore {
        return blocked(RepairPlanBlocker::NewerVersion);
    }
    if !observed.readable {
        return blocked(RepairPlanBlocker::UnreadableHome);
    }
    if observed.busy {
        return blocked(RepairPlanBlocker::Busy);
    }
    if observed.keychain_locked {
        return blocked(RepairPlanBlocker::Keychain);
    }
    if observed.foreign_process {
        return blocked(RepairPlanBlocker::ForeignProcess);
    }
    if needs_archive(observed.home) && observed.explicit_home {
        return blocked(RepairPlanBlocker::ExplicitHome);
    }
    let archive = needs_archive(observed.home);
    let setup = archive || observed.home == HomeState::Absent;
    let update = observed.service_present
        && !observed.managed_update
        && (observed.version == RepairVersion::Older
            || !observed.service_program_present
            || !observed.service_same_home);
    let stop =
        observed.service_running && (archive || observed.home == HomeState::Absent || update);
    let drain = stop && observed.home == HomeState::Healthy;
    let restart = update && observed.service_running && !setup;
    let mut steps = Vec::new();
    if drain {
        if observed.drain_supported {
            steps.push(RepairStep::Drain);
        }
        if !observed.drain_supported {
            steps.push(RepairStep::Backup);
        }
    }
    if stop {
        steps.push(RepairStep::StopOldNode);
    }
    if update && observed.home == HomeState::Healthy {
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
    RepairPlan {
        steps,
        blocker: RepairPlanBlocker::None,
        needs_setup: setup,
    }
}

pub fn refusal_action(observed: &RefusalObservation) -> RefusalAction {
    if !observed.fresh {
        RefusalAction::Ignore
    } else if observed.newer_store {
        RefusalAction::Block
    } else if observed.missing_key || observed.older_store {
        if observed.explicit_home {
            RefusalAction::Ask
        } else {
            RefusalAction::Archive
        }
    } else {
        RefusalAction::Block
    }
}
