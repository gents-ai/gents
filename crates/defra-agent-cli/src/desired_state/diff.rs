use std::collections::BTreeSet;
use std::path::Path;

use super::{
    DesiredAgentPrincipal, DesiredStateCollectionDiff, DesiredStateDiffCollections,
    DesiredStateDiffReport, DesiredStateManifest, HasUniqueId,
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
    let collections = DesiredStateDiffCollections {
        agent_principal,
        agent_behaviors: diff_manifest_collection(&desired.agent_behaviors, &live.agent_behaviors),
        tool_selections: diff_manifest_collection(&desired.tool_selections, &live.tool_selections),
        inference_backends: diff_manifest_collection(
            &desired.inference_backends,
            &live.inference_backends,
        ),
        inference_profiles: diff_manifest_collection(
            &desired.inference_profiles,
            &live.inference_profiles,
        ),
        tool_service_registries: diff_manifest_collection(
            &desired.tool_service_registries,
            &live.tool_service_registries,
        ),
        tasks: diff_manifest_collection(&desired.tasks, &live.tasks),
        schedules: diff_manifest_collection(&desired.schedules, &live.schedules),
        event_triggers: diff_manifest_collection(&desired.event_triggers, &live.event_triggers),
    };

    let counts = collections.counts();
    let ok = counts.is_exact_match();

    DesiredStateDiffReport {
        status: "diffed",
        ok,
        root: root.display().to_string(),
        access_mode: access_mode.to_string(),
        agent_did: desired.agent_principal.agent_did.clone(),
        counts,
        collections,
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

fn diff_manifest_collection<T>(desired: &[T], live: &[T]) -> DesiredStateCollectionDiff
where
    T: HasUniqueId + PartialEq,
{
    diff_collection(
        desired
            .iter()
            .map(|value| (value.unique_id().to_string(), value))
            .collect(),
        live.iter()
            .map(|value| (value.unique_id().to_string(), value))
            .collect(),
    )
}
