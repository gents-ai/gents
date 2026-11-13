use gents_protocol::operations::install_repair::{
    HomeState, RefusalAction, RefusalObservation, RepairObservation, RepairOperation, RepairPlan,
    RepairPlanBlocker, RepairStep, RepairVersion,
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
    let stop =
        observed.service_running && (archive || observed.home == HomeState::Absent || update);
    let drain = stop && observed.home == HomeState::Healthy;
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
    steps
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
