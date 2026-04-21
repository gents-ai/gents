use std::collections::BTreeSet;
use std::path::Path;

use super::{
    DesiredAgentPrincipal, DesiredStateCollectionDiff, DesiredStateDiffCollections,
    DesiredStateDiffCollectionsCounts, DesiredStateDiffReport, DesiredStateManifest,
};

pub(crate) fn diff_manifests(
    root: &Path,
    access_mode: &str,
    desired: &DesiredStateManifest,
    live_principal: Option<&DesiredAgentPrincipal>,
    live: &DesiredStateManifest,
) -> DesiredStateDiffReport {
    let agent_principal = diff_single(
        &desired.agent_principal.agent_did,
        Some(&desired.agent_principal),
        live_principal,
    );
    let agent_behaviors = diff_collection(
        desired
            .agent_behaviors
            .iter()
            .map(|value| (value.behavior_id.clone(), value))
            .collect(),
        live.agent_behaviors
            .iter()
            .map(|value| (value.behavior_id.clone(), value))
            .collect(),
    );
    let tool_selections = diff_collection(
        desired
            .tool_selections
            .iter()
            .map(|value| (value.selection_id.clone(), value))
            .collect(),
        live.tool_selections
            .iter()
            .map(|value| (value.selection_id.clone(), value))
            .collect(),
    );
    let inference_backends = diff_collection(
        desired
            .inference_backends
            .iter()
            .map(|value| (value.backend_id.clone(), value))
            .collect(),
        live.inference_backends
            .iter()
            .map(|value| (value.backend_id.clone(), value))
            .collect(),
    );
    let inference_profiles = diff_collection(
        desired
            .inference_profiles
            .iter()
            .map(|value| (value.profile_id.clone(), value))
            .collect(),
        live.inference_profiles
            .iter()
            .map(|value| (value.profile_id.clone(), value))
            .collect(),
    );
    let tool_service_registries = diff_collection(
        desired
            .tool_service_registries
            .iter()
            .map(|value| (value.service_id.clone(), value))
            .collect(),
        live.tool_service_registries
            .iter()
            .map(|value| (value.service_id.clone(), value))
            .collect(),
    );
    let tasks = diff_collection(
        desired
            .tasks
            .iter()
            .map(|value| (value.task_id.clone(), value))
            .collect(),
        live.tasks
            .iter()
            .map(|value| (value.task_id.clone(), value))
            .collect(),
    );
    let schedules = diff_collection(
        desired
            .schedules
            .iter()
            .map(|value| (value.schedule_id.clone(), value))
            .collect(),
        live.schedules
            .iter()
            .map(|value| (value.schedule_id.clone(), value))
            .collect(),
    );

    let counts = DesiredStateDiffCollectionsCounts {
        agent_principal: agent_principal.counts(),
        agent_behaviors: agent_behaviors.counts(),
        tool_selections: tool_selections.counts(),
        inference_backends: inference_backends.counts(),
        inference_profiles: inference_profiles.counts(),
        tool_service_registries: tool_service_registries.counts(),
        tasks: tasks.counts(),
        schedules: schedules.counts(),
    };
    let ok = [
        &counts.agent_principal,
        &counts.agent_behaviors,
        &counts.tool_selections,
        &counts.inference_backends,
        &counts.inference_profiles,
        &counts.tool_service_registries,
        &counts.tasks,
        &counts.schedules,
    ]
    .iter()
    .all(|count| count.create == 0 && count.update == 0 && count.live_only == 0);

    DesiredStateDiffReport {
        status: "diffed",
        ok,
        root: root.display().to_string(),
        access_mode: access_mode.to_string(),
        agent_did: desired.agent_principal.agent_did.clone(),
        counts,
        collections: DesiredStateDiffCollections {
            agent_principal,
            agent_behaviors,
            tool_selections,
            inference_backends,
            inference_profiles,
            tool_service_registries,
            tasks,
            schedules,
        },
    }
}

pub(super) fn diff_single<T>(
    id: &str,
    desired: Option<&T>,
    live: Option<&T>,
) -> DesiredStateCollectionDiff
where
    T: PartialEq,
{
    match (desired, live) {
        (Some(desired), Some(live)) => {
            if desired == live {
                DesiredStateCollectionDiff {
                    create: Vec::new(),
                    update: Vec::new(),
                    unchanged: vec![id.to_string()],
                    live_only: Vec::new(),
                }
            } else {
                DesiredStateCollectionDiff {
                    create: Vec::new(),
                    update: vec![id.to_string()],
                    unchanged: Vec::new(),
                    live_only: Vec::new(),
                }
            }
        }
        (Some(_), None) => DesiredStateCollectionDiff {
            create: vec![id.to_string()],
            update: Vec::new(),
            unchanged: Vec::new(),
            live_only: Vec::new(),
        },
        (None, Some(_)) => DesiredStateCollectionDiff {
            create: Vec::new(),
            update: Vec::new(),
            unchanged: Vec::new(),
            live_only: vec![id.to_string()],
        },
        (None, None) => DesiredStateCollectionDiff {
            create: Vec::new(),
            update: Vec::new(),
            unchanged: Vec::new(),
            live_only: Vec::new(),
        },
    }
}

pub(crate) fn diff_collection<T>(
    desired: Vec<(String, &T)>,
    live: Vec<(String, &T)>,
) -> DesiredStateCollectionDiff
where
    T: PartialEq,
{
    let desired_map = desired
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let live_map = live
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut create = Vec::new();
    let mut update = Vec::new();
    let mut unchanged = Vec::new();
    let mut live_only = Vec::new();

    let all_ids = desired_map
        .keys()
        .chain(live_map.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for id in all_ids {
        match (desired_map.get(&id), live_map.get(&id)) {
            (Some(desired), Some(live)) => {
                if *desired == *live {
                    unchanged.push(id);
                } else {
                    update.push(id);
                }
            }
            (Some(_), None) => create.push(id),
            (None, Some(_)) => live_only.push(id),
            (None, None) => {}
        }
    }

    DesiredStateCollectionDiff {
        create,
        update,
        unchanged,
        live_only,
    }
}
