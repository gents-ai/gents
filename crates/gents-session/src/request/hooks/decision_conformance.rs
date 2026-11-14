use super::effect::{DocRef, Manifest, OwnedCollection};
use super::hook::{history_permitted, Query};
use super::supersede::{Ancestry, BrokenLink, ParentFacts, PendingFacts, Selector};
use gents_protocol::request_input::GoalAssignmentInput;

fn reference(
    wire: &Option<gents_lean_vocab::families::hook_decisions_supersede::ReferenceWire>,
) -> Option<GoalAssignmentInput> {
    wire.as_ref().map(|reference| GoalAssignmentInput {
        goal_id: reference.goal.clone(),
        root_request_doc_id: reference.root.clone(),
    })
}

fn link(link: BrokenLink) -> &'static str {
    match link {
        BrokenLink::Unsigned => "unsigned",
        BrokenLink::OtherGoal => "other_goal",
        BrokenLink::NoParent => "no_parent",
        BrokenLink::ParentAbsent => "parent_absent",
        BrokenLink::ParentOutsideSession => "parent_outside_session",
        BrokenLink::ParentUnsigned => "parent_unsigned",
        BrokenLink::Diverges => "diverges",
    }
}

#[test]
fn supersede_retirements_follow_the_lean_selector() {
    gents_lean_vocab::families::hook_decisions_supersede::replay::<String, String>(|case| {
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
        gents_lean_vocab::families::hook_decisions_supersede::SupersedeOutput {
            stale: selector
                .stale(&rows, &parents)
                .into_iter()
                .map(
                    |retired| gents_lean_vocab::families::hook_decisions_supersede::StaleWire {
                        doc: retired.doc,
                        request: retired.request,
                        successor: retired.successor,
                        successor_doc: retired.successor_doc,
                        reason: retired.reason,
                        ancestry: match retired.ancestry {
                            Ancestry::Ordinary => {
                                gents_lean_vocab::families::hook_decisions_supersede::AncestryWire {
                                    kind: "ordinary".to_owned(),
                                    link: None,
                                }
                            }
                            Ancestry::Broken(broken) => {
                                gents_lean_vocab::families::hook_decisions_supersede::AncestryWire {
                                    kind: "broken".to_owned(),
                                    link: Some(link(broken).to_owned()),
                                }
                            }
                        },
                    },
                )
                .collect(),
        }
    });
}

#[test]
fn history_reads_follow_the_lean_history_bound() {
    gents_lean_vocab::families::hook_decisions_history::replay(|case| {
        let leak = |text: &str| -> &'static str { Box::leak(text.to_owned().into_boxed_str()) };
        let manifest = Manifest {
            name: "plugin",
            owns: case
                .owns
                .iter()
                .map(|collection| OwnedCollection {
                    name: leak(collection),
                    schema: None,
                    grants: &[],
                })
                .collect(),
            bound: 0,
            history: case
                .history
                .iter()
                .map(|collection| leak(collection))
                .collect(),
        };
        let named = case
            .named
            .iter()
            .map(|named| DocRef::new(named.collection.clone(), named.doc.clone()))
            .collect::<Vec<_>>();
        let mut query = Query::select(case.read.collection.clone(), "status").revisions("");
        if let Some(doc) = &case.read.doc {
            query = query.eq("_docID", doc.clone());
        }
        gents_lean_vocab::families::hook_decisions_history::HistoryOutput {
            permitted: history_permitted(&manifest, &named, &query),
        }
    });
}
