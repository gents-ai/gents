//! The agent supervisor's placement fence and the stop barrier built on it.
//!
//! A proposal carries the floor positions its view read ([`PlacementBasis`]).
//! After committing a floor, the assignment receiver raises the fence to it;
//! the supervisor drops every proposal whose basis is below the fence and
//! publishes the basis of each proposal it takes, together with the Agents of
//! its generation and the retired slots that have not joined
//! ([`PlacementProgress`]). [`PlacementFence::stopped`] waits on that progress.
//! The fence is held in memory only.

use std::collections::{BTreeMap, BTreeSet};

use defra_node::AssignmentPosition;
use tokio::sync::watch;

/// Floor positions per assignment scope key.
pub type PlacementFloors = BTreeMap<String, AssignmentPosition>;

/// What a runtime view read in the transaction that read its Agents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementBasis {
    pub floors: PlacementFloors,
    /// Agent `_docID` by agent DID.
    pub agent_documents: BTreeMap<String, String>,
}

impl PlacementBasis {
    fn document<'a>(&'a self, agent_did: &'a str) -> &'a str {
        self.agent_documents
            .get(agent_did)
            .map_or(agent_did, String::as_str)
    }
}

/// Lean `RuntimeReconcile.Placement.reached`.
pub fn reached(basis: &AssignmentPosition, floor: &AssignmentPosition) -> bool {
    floor.term < basis.term || (floor.term == basis.term && floor.height <= basis.height)
}

fn read_floor(floors: &PlacementFloors, scope: &str, floor: &AssignmentPosition) -> bool {
    floors
        .get(scope)
        .is_some_and(|position| reached(position, floor))
}

/// Lean `RuntimeReconcile.Placement.admitsProposal`.
pub fn admits_proposal(basis: &PlacementFloors, fence: &PlacementFloors) -> bool {
    fence
        .iter()
        .all(|(scope, floor)| read_floor(basis, scope, floor))
}

/// What the supervisor has processed, with Agents named by `_docID`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlacementProgress {
    pub basis: PlacementBasis,
    pub generation_agents: BTreeSet<String>,
    /// Retired slots that have not joined, by agent DID and slot generation.
    pub retiring: BTreeMap<(String, u64), String>,
}

impl PlacementProgress {
    fn new(basis: &PlacementBasis, agent_dids: &[String]) -> Self {
        Self {
            generation_agents: agent_dids
                .iter()
                .map(|did| basis.document(did).to_owned())
                .collect(),
            basis: basis.clone(),
            retiring: BTreeMap::new(),
        }
    }

    /// Lean `RuntimeReconcile.Placement.stopped`.
    pub fn stopped(&self, scope: &str, floor: &AssignmentPosition, ids: &[String]) -> bool {
        read_floor(&self.basis.floors, scope, floor)
            && ids.iter().all(|id| {
                !self.generation_agents.contains(id) && !self.retiring.values().any(|r| r == id)
            })
    }
}

pub struct PlacementFence {
    fence: watch::Sender<PlacementFloors>,
    progress: watch::Sender<PlacementProgress>,
    resolve: watch::Sender<u64>,
}

impl PlacementFence {
    /// A fence over a supervisor that bootstrapped `agent_dids` from `basis`.
    pub fn new(basis: &PlacementBasis, agent_dids: &[String]) -> Self {
        Self {
            fence: watch::Sender::new(PlacementFloors::new()),
            progress: watch::Sender::new(PlacementProgress::new(basis, agent_dids)),
            resolve: watch::Sender::new(0),
        }
    }

    /// Raises the fence for `scope` to `floor` and asks the control watcher to
    /// resolve again, since the views it holds may be older than the floor.
    pub fn raise(&self, scope: &str, floor: &AssignmentPosition) {
        self.fence.send_if_modified(|fence| {
            if fence
                .get(scope)
                .is_some_and(|current| reached(current, floor))
            {
                return false;
            }
            fence.insert(scope.to_owned(), floor.clone());
            true
        });
        self.request_resolve();
    }

    pub fn admits(&self, basis: &PlacementBasis) -> bool {
        admits_proposal(&basis.floors, &self.fence.borrow())
    }

    /// Resolves once the supervisor has taken a proposal whose basis reached
    /// `floor` for `scope`, and no Agent of `ids` (`_docID`s) is in its
    /// generation or in a retired slot that has not joined.
    pub async fn stopped(&self, scope: &str, floor: &AssignmentPosition, ids: &[String]) {
        let mut progress = self.progress.subscribe();
        let _ = progress
            .wait_for(|progress| progress.stopped(scope, floor, ids))
            .await;
    }

    pub fn progress(&self) -> watch::Receiver<PlacementProgress> {
        self.progress.subscribe()
    }

    /// Changes each time the control watcher should resolve again.
    pub fn resolve_requests(&self) -> watch::Receiver<u64> {
        self.resolve.subscribe()
    }

    pub fn request_resolve(&self) {
        self.resolve
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Publishes a new generation built from `basis` and the slots it retired,
    /// in one update so no observer sees a retired Agent in neither set.
    pub fn publish_generation(
        &self,
        basis: &PlacementBasis,
        agent_dids: &[String],
        retired: &[(String, u64)],
    ) {
        self.progress.send_modify(|progress| {
            for (agent_did, generation) in retired {
                let document = progress.basis.document(agent_did).to_owned();
                progress
                    .retiring
                    .insert((agent_did.clone(), *generation), document);
            }
            let retiring = std::mem::take(&mut progress.retiring);
            *progress = PlacementProgress {
                retiring,
                ..PlacementProgress::new(basis, agent_dids)
            };
        });
    }

    /// An unchanged configuration: the generation stays, the basis moves.
    pub fn advance_basis(&self, basis: &PlacementBasis) {
        self.progress.send_if_modified(|progress| {
            if progress.basis == *basis {
                return false;
            }
            progress.basis = basis.clone();
            true
        });
    }

    pub fn joined(&self, agent_did: &str, generation: u64) {
        self.progress.send_if_modified(|progress| {
            progress
                .retiring
                .remove(&(agent_did.to_owned(), generation))
                .is_some()
        });
    }
}

#[cfg(test)]
mod tests;
