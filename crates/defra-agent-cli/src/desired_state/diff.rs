use std::collections::BTreeSet;
use std::path::Path;

use super::{
    DesiredAgentPrincipal, DesiredStateCollectionDiff, DesiredStateDiffCollections,
    DesiredStateDiffReport, DesiredStateManifest, HasUniqueId,
};
use defra_agent::Collection;

pub(crate) fn diff_manifests(
    root: &Path,
    access_mode: &str,
    desired: &DesiredStateManifest,
    live_principal: Option<&DesiredAgentPrincipal>,
    live: &DesiredStateManifest,
) -> DesiredStateDiffReport {
    diff_manifests_inner(root, access_mode, desired, live_principal, live, false)
}

pub(crate) fn diff_manifests_with_prune(
    root: &Path,
    access_mode: &str,
    desired: &DesiredStateManifest,
    live_principal: Option<&DesiredAgentPrincipal>,
    live: &DesiredStateManifest,
) -> DesiredStateDiffReport {
    diff_manifests_inner(root, access_mode, desired, live_principal, live, true)
}

fn diff_manifests_inner(
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

    if prune {
        mark_delete_safe_live_only_docs(&mut collections, live);
    }

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

fn mark_delete_safe_live_only_docs(
    collections: &mut DesiredStateDiffCollections,
    live: &DesiredStateManifest,
) {
    let live_references = live_structural_references(live);
    for collection in Collection::ALL {
        let diff = collections.get_mut(collection);
        diff.delete = diff
            .live_only
            .iter()
            .filter(|id| !live_references.contains(&(collection, (*id).clone())))
            .cloned()
            .collect();
    }
}

fn live_structural_references(live: &DesiredStateManifest) -> BTreeSet<(Collection, String)> {
    let mut references = BTreeSet::new();

    insert_optional_reference(
        &mut references,
        Collection::AgentBehavior,
        live.agent_principal.default_behavior_id.as_deref(),
    );

    for behavior in &live.agent_behaviors {
        insert_optional_reference(
            &mut references,
            Collection::InferenceBackend,
            behavior.backend_id.as_deref(),
        );
        insert_optional_reference(
            &mut references,
            Collection::ToolSelection,
            behavior.tool_selection_id.as_deref(),
        );
        insert_optional_reference(
            &mut references,
            Collection::InferenceProfile,
            behavior.inference_profile_id.as_deref(),
        );
        insert_reference_values(&mut references, Collection::Skill, &behavior.skill_refs);
        insert_reference_values(&mut references, Collection::Skill, &behavior.skill_excludes);
    }

    for skill in &live.skills {
        insert_reference_values(
            &mut references,
            Collection::ToolServiceRegistry,
            &skill.tool_refs,
        );
    }

    for selection in &live.tool_selections {
        insert_reference_values(
            &mut references,
            Collection::ToolServiceRegistry,
            &selection.allowed_mcp_service_ids,
        );
    }

    for task in &live.tasks {
        insert_reference(
            &mut references,
            Collection::AgentBehavior,
            &task.behavior_id,
        );
    }

    for schedule in &live.schedules {
        insert_reference(&mut references, Collection::Task, &schedule.task_id);
    }

    for trigger in &live.event_triggers {
        insert_reference(&mut references, Collection::Task, &trigger.task_id);
    }

    references
}

fn insert_optional_reference(
    references: &mut BTreeSet<(Collection, String)>,
    collection: Collection,
    value: Option<&str>,
) {
    if let Some(value) = value {
        insert_reference(references, collection, value);
    }
}

fn insert_reference_values(
    references: &mut BTreeSet<(Collection, String)>,
    collection: Collection,
    values: &[String],
) {
    for value in values {
        insert_reference(references, collection, value);
    }
}

fn insert_reference(
    references: &mut BTreeSet<(Collection, String)>,
    collection: Collection,
    value: &str,
) {
    let value = value.trim();
    if !value.is_empty() {
        references.insert((collection, value.to_string()));
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
