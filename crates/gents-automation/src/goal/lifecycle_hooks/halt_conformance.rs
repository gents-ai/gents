use super::{halt_verdict, HaltVerdict};
use crate::goal::GoalState;
use gents_protocol::goal_status::GoalStatus;

#[test]
fn halt_verdicts_follow_the_lean_goal_halt_model() {
    gents_lean_vocab::families::hook_decisions_goal_halt::replay::<String>(|case| {
        let state = case.status.as_deref().map(|status| GoalState {
            status: GoalStatus::parse(status).expect("the model names a known status"),
            blocked_audits: 0,
            wrapup_requested: false,
            wrapup_completed: false,
        });
        let verdict = halt_verdict(
            state,
            case.updated.as_ref().map(|stamp| stamp.text.as_str()),
            &case.halt.text,
        );
        let (kind, paused) = match verdict {
            HaltVerdict::Inactive => ("inactive", None),
            HaltVerdict::ChangedAfter => ("changed_after", None),
            HaltVerdict::UnreadableHalt => ("unreadable_halt", None),
            HaltVerdict::Pause(paused) => (
                "pause",
                Some(
                    gents_lean_vocab::families::hook_decisions_goal_halt::PausedGoalWire {
                        status: paused.status.as_str().to_owned(),
                        blocked_audits: u64::try_from(paused.blocked_audits).unwrap(),
                        wrapup_requested: paused.wrapup_requested,
                        wrapup_completed: paused.wrapup_completed,
                    },
                ),
            ),
        };
        gents_lean_vocab::families::hook_decisions_goal_halt::GoalHaltOutput {
            kind: kind.to_owned(),
            paused,
        }
    });
}
