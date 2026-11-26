use super::*;
use gents_lean_vocab::families::{placement_admits_proposal, placement_stopped};

fn position(position: &placement_stopped::Position) -> AssignmentPosition {
    AssignmentPosition {
        term: position.term,
        height: position.height,
        block_hash: position.hash.clone(),
    }
}

fn floors(entries: &[placement_stopped::Floor]) -> PlacementFloors {
    entries
        .iter()
        .map(|entry| (entry.scope.clone(), position(&entry.position)))
        .collect()
}

#[test]
fn proposal_admission_replays_the_lean_owner() {
    placement_admits_proposal::replay(|input| {
        let floors = |entries: &[placement_admits_proposal::Floor]| -> PlacementFloors {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry.scope.clone(),
                        AssignmentPosition {
                            term: entry.position.term,
                            height: entry.position.height,
                            block_hash: entry.position.hash.clone(),
                        },
                    )
                })
                .collect()
        };
        admits_proposal(&floors(&input.basis), &floors(&input.fence))
    });
}

#[test]
fn the_stop_barrier_replays_the_lean_owner() {
    placement_stopped::replay(|input| {
        let progress = PlacementProgress {
            basis: PlacementBasis {
                floors: floors(&input.basis),
                agent_documents: BTreeMap::new(),
            },
            generation_agents: input.generation.iter().cloned().collect(),
            retiring: input
                .retiring
                .iter()
                .enumerate()
                .map(|(slot, id)| ((format!("did:{id}"), slot as u64), id.clone()))
                .collect(),
        };
        progress.stopped(&input.scope, &position(&input.floor), &input.ids)
    });
}
