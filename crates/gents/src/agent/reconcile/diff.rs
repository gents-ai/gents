use crate::runtime_snapshot::{ActiveRuntimeSnapshot, ResolvedRuntimeSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SnapshotDiffCounts {
    pub(super) added: usize,
    pub(super) removed: usize,
    pub(super) updated: usize,
    pub(super) default_changed: bool,
    pub(super) unavailable_changed: bool,
}

pub(super) fn diff_counts(
    current: &ActiveRuntimeSnapshot,
    proposed: &ResolvedRuntimeSnapshot,
) -> SnapshotDiffCounts {
    let current_ids = current
        .agents
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let proposed_ids = proposed
        .agents
        .keys()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let added = proposed_ids.difference(&current_ids).count();
    let removed = current_ids.difference(&proposed_ids).count();
    let updated = current_ids
        .intersection(&proposed_ids)
        .filter(|agent_id| {
            let current_agent = current
                .agents
                .get(*agent_id)
                .expect("intersection must exist in current agents");
            let proposed_agent = proposed
                .agents
                .get(*agent_id)
                .expect("intersection must exist in proposed agents");
            format!("{current_agent:?}") != format!("{proposed_agent:?}")
                || match (
                    current.tool_surfaces.get(*agent_id),
                    proposed.tool_surfaces.get(*agent_id),
                ) {
                    (Some(current_tools), Some(proposed_tools)) => {
                        format!("{current_tools:?}") != format!("{proposed_tools:?}")
                    }
                    _ => true,
                }
        })
        .count();

    SnapshotDiffCounts {
        added,
        removed,
        updated,
        default_changed: current.default_agent_id != proposed.default_agent_id,
        unavailable_changed: current.unavailable_agents != proposed.unavailable_agents,
    }
}
