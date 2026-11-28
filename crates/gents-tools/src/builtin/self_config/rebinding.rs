use serde_json::{json, Value};

use super::ops::PatchOutcome;
use crate::tools_rebinding::ToolsRebinding;

const IN_EFFECT: &str =
    "In effect now for your next tool call in this same response (keep working, do not end your reply): you can call next_turn.added and no longer have next_turn.removed.";
const WAITING: &str = "In effect now for your next tool call in this same response (keep working, do not end your reply), except next_turn.waiting which stays unavailable until the reason given is resolved.";
const UNRUNNABLE: &str = "Saved, but the agent cannot run with these Tools (next_turn.not_applied), so this request keeps its tools. Correct them with tools update.";
const UNPUBLISHED: &str = "Saved; the runtime has not picked it up yet, so this request keeps its tools and the next request has the change.";

/// The receipt of a committed Tools write, with what it did to the calling
/// request. A receipt for another agent's Tools keeps its timing note.
pub(super) fn applied(mut outcome: PatchOutcome, rebinding: ToolsRebinding) -> PatchOutcome {
    if outcome.effect == super::ops::EFFECT_TIMING_NOTE {
        if let Some(effect) = effect(&rebinding) {
            outcome.effect = effect;
        }
    }
    outcome.next_turn = next_turn(&rebinding);
    outcome
}

pub(super) fn effect(rebinding: &ToolsRebinding) -> Option<&'static str> {
    match rebinding {
        ToolsRebinding::NotOwn => None,
        ToolsRebinding::Rebound { waiting, .. } if !waiting.is_empty() => Some(WAITING),
        ToolsRebinding::Rebound { .. } => Some(IN_EFFECT),
        ToolsRebinding::Unrunnable { .. } => Some(UNRUNNABLE),
        ToolsRebinding::Unpublished => Some(UNPUBLISHED),
    }
}

pub(super) fn next_turn(rebinding: &ToolsRebinding) -> Option<Value> {
    match rebinding {
        ToolsRebinding::NotOwn => None,
        ToolsRebinding::Rebound {
            added,
            removed,
            waiting,
        } if !waiting.is_empty() => Some(json!({
            "added": added,
            "removed": removed,
            "waiting": waiting
                .iter()
                .map(|item| json!({"name": item.name, "why": item.reason}))
                .collect::<Vec<_>>(),
        })),
        ToolsRebinding::Rebound { added, removed, .. } => {
            Some(json!({"added": added, "removed": removed}))
        }
        ToolsRebinding::Unrunnable { reason } => Some(json!({"not_applied": reason})),
        ToolsRebinding::Unpublished => {
            Some(json!({"not_applied": "no runtime generation holds it yet"}))
        }
    }
}
