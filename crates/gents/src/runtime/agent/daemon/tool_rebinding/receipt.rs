use gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot;
use gents_tools::surface::ToolSurface;
use gents_tools::tools_rebinding::{ToolsRebinding, WaitingItem};

/// Lean `RuntimeReconcile.ToolRebinding.Change`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Change {
    Added,
    Removed,
    Waiting,
    Unchanged,
}

/// Lean `RuntimeReconcile.ToolRebinding.change`.
pub(super) fn change(held: bool, selected: bool, offered: bool) -> Change {
    match (offered, selected, held) {
        (true, _, true) | (false, false, false) => Change::Unchanged,
        (true, _, false) => Change::Added,
        (false, true, _) => Change::Waiting,
        (false, false, true) => Change::Removed,
    }
}

/// What a request's toolset offers the model: its advertised tools and the
/// agent targets `agent_new` takes.
#[derive(Clone, Default)]
pub(super) struct Held {
    pub(super) tools: Vec<String>,
    pub(super) targets: Vec<String>,
}

impl Held {
    pub(super) fn offered_by(tools: Vec<String>, surface: &ToolSurface) -> Self {
        Self {
            tools,
            targets: surface
                .agent_targets()
                .iter()
                .map(|target| target.name.clone())
                .collect(),
        }
    }
}

/// The receipt of replacing `held` with `adopted`, built from `surface` in
/// `snapshot`: each tool and agent target the change added, removed or left
/// waiting on its agent.
pub(super) fn rebound(
    held: &Held,
    adopted: &Held,
    surface: &ToolSurface,
    snapshot: &ActiveRuntimeSnapshot,
) -> ToolsRebinding {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut waiting = Vec::new();
    for name in union([&held.tools, &adopted.tools]) {
        let offered = adopted.tools.contains(&name);
        match change(held.tools.contains(&name), offered, offered) {
            Change::Added => added.push(name),
            Change::Removed => removed.push(name),
            Change::Waiting | Change::Unchanged => {}
        }
    }
    let withheld = surface.withheld_agent_targets();
    let withheld_names = withheld
        .iter()
        .map(|target| target.name.clone())
        .collect::<Vec<_>>();
    for name in union([&held.targets, &adopted.targets, &withheld_names]) {
        let offered = adopted.targets.contains(&name);
        let label = format!("agent_new agent={name}");
        match change(
            held.targets.contains(&name),
            offered || withheld_names.contains(&name),
            offered,
        ) {
            Change::Added => added.push(label),
            Change::Removed => removed.push(label),
            Change::Waiting => waiting.push(WaitingItem {
                name: label,
                reason: withheld
                    .iter()
                    .find(|target| target.name == name)
                    .and_then(|target| snapshot.unavailable_agents.get(&target.agent_did))
                    .map_or_else(
                        || "its agent does not run".to_owned(),
                        |agent| agent.rejection_text().to_owned(),
                    ),
            }),
            Change::Unchanged => {}
        }
    }
    ToolsRebinding::Rebound {
        added,
        removed,
        waiting,
    }
}

fn union<const N: usize>(lists: [&Vec<String>; N]) -> Vec<String> {
    let mut names = Vec::new();
    for name in lists.into_iter().flatten() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}
