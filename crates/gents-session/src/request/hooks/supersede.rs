use serde::{Deserialize, Serialize};

use gents_protocol::goal_names::GOAL_TRIGGER_KIND;
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::identity::request_receipt::verify_request_receipt_signature;

/// Lean `Hooks.Selector`: the Goal assignment whose members a session keeps,
/// the successor and ordinary reason core records on every other pending Goal
/// continuation of the session, and the next call the plugin names for one
/// whose ancestry is broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selector {
    pub node_did: String,
    pub session_id: String,
    pub goal_id: String,
    pub root_doc_id: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
    #[serde(default)]
    pub next: Option<String>,
    pub at: String,
}

/// Lean `Hooks.BrokenLink`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokenLink {
    Unsigned,
    OtherGoal,
    NoParent,
    ParentAbsent,
    ParentOutsideSession,
    ParentUnsigned,
    Diverges,
}

impl BrokenLink {
    /// Lean `Hooks.BrokenLink.describe`.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Unsigned => "its receipt signature is missing or invalid",
            Self::OtherGoal => "its signed Goal reference names another Goal",
            Self::NoParent => "it names no parent request",
            Self::ParentAbsent => "its parent request is not visible",
            Self::ParentOutsideSession => "its parent request is in another node or session",
            Self::ParentUnsigned => "its parent's receipt signature is missing or invalid",
            Self::Diverges => "its signed Goal reference does not continue its parent's",
        }
    }
}

/// Lean `Hooks.Ancestry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ancestry {
    Ordinary,
    Broken(BrokenLink),
}

/// Lean `Hooks.PendingRow`: what core reads of a session request for a
/// supersede selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFacts {
    pub doc: String,
    pub request: String,
    pub pending: bool,
    pub goal_continuation: bool,
    pub trigger: Option<String>,
    pub reference: Option<GoalAssignmentInput>,
    pub signed: bool,
    pub parent: Option<String>,
}

impl PendingFacts {
    /// The row's facts, its receipt checked by the store's verifier.
    pub fn of(row: &AgentRequestRow, doc: &str) -> Self {
        Self {
            doc: doc.to_owned(),
            request: row.request_id.clone(),
            pending: row.lifecycle_state == Some(RequestLifecycleState::Pending),
            goal_continuation: row.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND),
            trigger: row.caused_by_trigger_id.clone(),
            reference: row
                .input
                .as_ref()
                .and_then(|input| input.goal_assignment.clone()),
            signed: verify_request_receipt_signature(row).is_ok(),
            parent: row.caused_by_parent_request_doc_id.clone(),
        }
    }
}

/// Lean `Hooks.ParentRow`: the parent a retired continuation names, as core
/// reads it by document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentFacts {
    pub doc: String,
    pub in_session: bool,
    pub signed: bool,
    pub goal: Option<String>,
    pub reference: Option<GoalAssignmentInput>,
}

impl ParentFacts {
    pub fn of(row: &AgentRequestRow, doc: &str, node_did: &str, session_id: &str) -> Self {
        Self {
            doc: doc.to_owned(),
            in_session: row.node_did.as_deref() == Some(node_did)
                && row.session_id.as_deref() == Some(session_id),
            signed: verify_request_receipt_signature(row).is_ok(),
            goal: row
                .caused_by_trigger_id
                .clone()
                .filter(|_| row.caused_by_trigger_kind.as_deref() == Some(GOAL_TRIGGER_KIND)),
            reference: row
                .input
                .as_ref()
                .and_then(|input| input.goal_assignment.clone()),
        }
    }
}

/// Lean `Hooks.continues`.
fn continues(row: &PendingFacts, parent: &ParentFacts) -> bool {
    let goal = row.trigger.as_deref();
    parent
        .goal
        .as_deref()
        .is_none_or(|parent_goal| Some(parent_goal) == goal)
        && match &parent.reference {
            Some(reference) => {
                Some(reference.goal_id.as_str()) == goal
                    && row.reference.as_ref() == Some(&reference.continuation(&parent.doc))
            }
            None => row.reference.as_ref().is_none_or(|reference| {
                Some(reference.goal_id.as_str()) == goal
                    && reference
                        .root_request_doc_id
                        .as_deref()
                        .is_none_or(|root| root == parent.doc)
            }),
        }
}

/// Lean `Hooks.classify`.
pub fn classify(row: &PendingFacts, parents: &[ParentFacts]) -> Ancestry {
    if !row.signed {
        return Ancestry::Broken(BrokenLink::Unsigned);
    }
    if row
        .reference
        .as_ref()
        .is_some_and(|reference| Some(reference.goal_id.as_str()) != row.trigger.as_deref())
    {
        return Ancestry::Broken(BrokenLink::OtherGoal);
    }
    let Some(doc) = row.parent.as_deref() else {
        return Ancestry::Broken(BrokenLink::NoParent);
    };
    let Some(parent) = parents.iter().find(|parent| parent.doc == doc) else {
        return Ancestry::Broken(BrokenLink::ParentAbsent);
    };
    if !parent.in_session {
        Ancestry::Broken(BrokenLink::ParentOutsideSession)
    } else if !parent.signed {
        Ancestry::Broken(BrokenLink::ParentUnsigned)
    } else if !continues(row, parent) {
        Ancestry::Broken(BrokenLink::Diverges)
    } else {
        Ancestry::Ordinary
    }
}

/// Lean `Hooks.StaleRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retirement {
    pub doc: String,
    pub request: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
    pub ancestry: Ancestry,
}

impl Selector {
    /// Lean `Hooks.Selector.member`, from the row's own signed fields.
    fn member(&self, row: &PendingFacts) -> bool {
        row.doc == self.root_doc_id
            || (row.trigger.as_deref() == Some(self.goal_id.as_str())
                && row.reference.as_ref().is_some_and(|reference| {
                    reference.goal_id == self.goal_id
                        && reference.root_request_doc_id.as_deref().unwrap_or(&row.doc)
                            == self.root_doc_id
                }))
    }

    /// Lean `Hooks.Selector.matches`.
    pub fn matches(&self, row: &PendingFacts) -> bool {
        row.pending && row.goal_continuation && !self.member(row)
    }

    /// Lean `Hooks.Selector.reasonFor`.
    pub fn reason_for(&self, ancestry: Ancestry) -> String {
        match ancestry {
            Ancestry::Ordinary => self.reason.clone(),
            Ancestry::Broken(link) => {
                let next = self
                    .next
                    .as_deref()
                    .map(|next| format!("; next: {next}"))
                    .unwrap_or_default();
                format!(
                    "{}; Goal ancestry broken: {}{next}",
                    self.reason,
                    link.describe()
                )
            }
        }
    }

    /// Lean `Hooks.Selector.stale`: the retirements this selector names on
    /// the session's rows, with the parents core read for them.
    pub fn stale(&self, rows: &[PendingFacts], parents: &[ParentFacts]) -> Vec<Retirement> {
        rows.iter()
            .filter(|row| self.matches(row))
            .map(|row| {
                let ancestry = classify(row, parents);
                Retirement {
                    doc: row.doc.clone(),
                    request: row.request.clone(),
                    successor: self.successor.clone(),
                    successor_doc: self.successor_doc.clone(),
                    reason: self.reason_for(ancestry),
                    ancestry,
                }
            })
            .collect()
    }
}
