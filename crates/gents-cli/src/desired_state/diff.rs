use std::collections::BTreeSet;
use std::path::Path;

use super::{
    DesiredAgentPrincipal, DesiredChainKeyBinding, DesiredStateCollectionDiff,
    DesiredStateDiffCollections, DesiredStateDiffReport, DesiredStateManifest, HasUniqueId,
};

pub(crate) fn diff_manifests(
    root: &Path,
    access_mode: &str,
    desired: &DesiredStateManifest,
    live_principal: Option<&DesiredAgentPrincipal>,
    live: &DesiredStateManifest,
    prune: bool,
) -> DesiredStateDiffReport {
    let agent_principal = diff_single(
        &desired.agent_principal.agent_did,
        Some(&desired.agent_principal),
        live_principal,
    );
    let mut collections = DesiredStateDiffCollections {
        agent_principal,
        agent_behaviors: diff_manifest_collection(&desired.agent_behaviors, &live.agent_behaviors),
        skills: diff_manifest_collection(&desired.skills, &live.skills),
        datastore_tool_surfaces: diff_manifest_collection(
            &desired.datastore_tool_surfaces,
            &live.datastore_tool_surfaces,
        ),
        chain_key_bindings: diff_chain_key_bindings(
            &desired.chain_key_bindings,
            &live.chain_key_bindings,
        ),
        eth_tools: diff_manifest_collection(&desired.eth_tools, &live.eth_tools),
        // WorkspaceRoot isn't tracked in DesiredStateManifest yet (see the
        // field doc on DesiredStateDiffCollections::workspace_roots) — an
        // empty diff until that CRUD surface lands.
        workspace_roots: DesiredStateCollectionDiff {
            create: Vec::new(),
            update: Vec::new(),
            delete: Vec::new(),
            unchanged: Vec::new(),
            live_only: Vec::new(),
        },
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
        projection_acp_bindings: diff_manifest_collection(
            &desired.projection_acp_bindings,
            &live.projection_acp_bindings,
        ),
        tasks: diff_manifest_collection(&desired.tasks, &live.tasks),
        schedules: diff_manifest_collection(&desired.schedules, &live.schedules),
        event_triggers: diff_manifest_collection(&desired.event_triggers, &live.event_triggers),
    };

    if prune {
        let deletes = super::prune::prune_safe_deletes(desired, live);
        collections.record_prune_deletes(&deletes);
    }

    let counts = collections.counts();
    let ok = counts.is_exact_match();

    DesiredStateDiffReport {
        status: "diffed",
        ok,
        root: root.display().to_string(),
        access_mode: access_mode.to_string(),
        agent_did: desired.agent_principal.agent_did.clone(),
        live_validation_errors: Vec::new(),
        counts,
        collections,
    }
}

fn diff_chain_key_bindings(
    desired: &[DesiredChainKeyBinding],
    live: &[DesiredChainKeyBinding],
) -> DesiredStateCollectionDiff {
    let mut comparable_live = live.to_vec();
    for live_binding in &mut comparable_live {
        if desired.iter().any(|desired_binding| {
            desired_binding.binding_id == live_binding.binding_id
                && desired_binding.revoked_at.is_none()
        }) {
            live_binding.revoked_at = None;
        }
    }
    diff_manifest_collection(desired, &comparable_live)
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
                    delete: Vec::new(),
                    unchanged: vec![id.to_string()],
                    live_only: Vec::new(),
                }
            } else {
                DesiredStateCollectionDiff {
                    create: Vec::new(),
                    update: vec![id.to_string()],
                    delete: Vec::new(),
                    unchanged: Vec::new(),
                    live_only: Vec::new(),
                }
            }
        }
        (Some(_), None) => DesiredStateCollectionDiff {
            create: vec![id.to_string()],
            update: Vec::new(),
            delete: Vec::new(),
            unchanged: Vec::new(),
            live_only: Vec::new(),
        },
        (None, Some(_)) => DesiredStateCollectionDiff {
            create: Vec::new(),
            update: Vec::new(),
            delete: Vec::new(),
            unchanged: Vec::new(),
            live_only: vec![id.to_string()],
        },
        (None, None) => DesiredStateCollectionDiff {
            create: Vec::new(),
            update: Vec::new(),
            delete: Vec::new(),
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
        delete: Vec::new(),
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
