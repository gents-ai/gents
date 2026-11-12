use gents_automation::goal::lifecycle_hooks::{halt_verdict, HaltVerdict};
use gents_automation::goal::GoalState;
use gents_lean_vocab::{
    lean_lifecycle_hook_goal_halt_cases, lean_lifecycle_hook_supersede_cases, LeanGoalHaltVerdict, LeanSupersedeAncestry,
    LeanSupersedeLink, LeanSupersedeReference, LeanSupersedeStale,
};
use gents_protocol::goal_status::GoalStatus;
use gents_protocol::request_input::GoalAssignmentInput;
use gents_session::request::hooks::supersede::{
    Ancestry, BrokenLink, ParentFacts, PendingFacts, Retirement, Selector,
};

fn reference(lean: &Option<LeanSupersedeReference>) -> Option<GoalAssignmentInput> {
    lean.as_ref().map(|reference| GoalAssignmentInput {
        goal_id: reference.goal.clone(),
        root_request_doc_id: reference.root.clone(),
    })
}

fn link(lean: LeanSupersedeLink) -> BrokenLink {
    match lean {
        LeanSupersedeLink::Unsigned => BrokenLink::Unsigned,
        LeanSupersedeLink::OtherGoal => BrokenLink::OtherGoal,
        LeanSupersedeLink::NoParent => BrokenLink::NoParent,
        LeanSupersedeLink::ParentAbsent => BrokenLink::ParentAbsent,
        LeanSupersedeLink::ParentOutsideSession => BrokenLink::ParentOutsideSession,
        LeanSupersedeLink::ParentUnsigned => BrokenLink::ParentUnsigned,
        LeanSupersedeLink::Diverges => BrokenLink::Diverges,
    }
}

fn retirement(lean: &LeanSupersedeStale) -> Retirement {
    Retirement {
        doc: lean.doc.clone(),
        request: lean.request.clone(),
        successor: lean.successor.clone(),
        successor_doc: lean.successor_doc.clone(),
        reason: lean.reason.clone(),
        ancestry: match lean.ancestry {
            LeanSupersedeAncestry::Ordinary => Ancestry::Ordinary,
            LeanSupersedeAncestry::Broken { link: lean } => Ancestry::Broken(link(lean)),
        },
    }
}

#[test]
fn generated_lifecycle_hook_supersede_cases_drive_the_retirement_classifier() {
    let cases = lean_lifecycle_hook_supersede_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let selector = Selector {
            node_did: "did:key:z6MkSupersedeNode".into(),
            session_id: "session-1".into(),
            goal_id: case.selector.goal.clone(),
            root_doc_id: case.selector.root.clone(),
            successor: case.selector.successor.clone(),
            successor_doc: case.selector.successor_doc.clone(),
            reason: case.selector.reason.clone(),
            next: case.selector.next.clone(),
            at: "2026-11-11T09:00:00Z".into(),
        };
        let rows = case
            .rows
            .iter()
            .map(|row| PendingFacts {
                doc: row.doc.clone(),
                request: row.request.clone(),
                pending: row.pending,
                goal_continuation: row.goal_continuation,
                trigger: row.trigger.clone(),
                reference: reference(&row.reference),
                signed: row.signed,
                parent: row.parent.clone(),
            })
            .collect::<Vec<_>>();
        let parents = case
            .parents
            .iter()
            .map(|parent| ParentFacts {
                doc: parent.doc.clone(),
                in_session: parent.in_session,
                signed: parent.signed,
                goal: parent.goal.clone(),
                reference: reference(&parent.reference),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            selector.stale(&rows, &parents),
            case.stale.iter().map(retirement).collect::<Vec<_>>(),
            "{}",
            case.name
        );
    }
}

fn verdict(lean: &LeanGoalHaltVerdict) -> HaltVerdict {
    match lean {
        LeanGoalHaltVerdict::Inactive => HaltVerdict::Inactive,
        LeanGoalHaltVerdict::ChangedAfter => HaltVerdict::ChangedAfter,
        LeanGoalHaltVerdict::UnreadableHalt => HaltVerdict::UnreadableHalt,
        LeanGoalHaltVerdict::Pause { status } => HaltVerdict::Pause(GoalState {
            status: GoalStatus::parse(status).expect("the model pauses to a known status"),
            blocked_audits: 0,
            wrapup_requested: false,
            wrapup_completed: false,
        }),
    }
}

#[test]
fn generated_lifecycle_hook_goal_halt_cases_drive_the_halt_verdict() {
    let cases = lean_lifecycle_hook_goal_halt_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let state = case.status.as_deref().map(|status| GoalState {
            status: GoalStatus::parse(status).expect("the model names a known status"),
            blocked_audits: 0,
            wrapup_requested: false,
            wrapup_completed: false,
        });
        assert_eq!(
            halt_verdict(
                state,
                case.updated.as_ref().map(|stamp| stamp.text.as_str()),
                &case.halt.text,
            ),
            verdict(&case.verdict),
            "{}",
            case.name
        );
    }
}
