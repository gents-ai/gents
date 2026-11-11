use super::{reissue_decision, request_lookup};

#[test]
fn request_lookup_matches_lean() {
    gents_lean_vocab::families::session_operations_request_lookup::replay(|i| {
        request_lookup(i.found)
    });
}

#[test]
fn reissue_decision_matches_lean() {
    gents_lean_vocab::families::session_operations_reissue_decision::replay(|i| {
        reissue_decision(
            i.kind,
            i.parent,
            i.interactive,
            i.budget_remains,
            i.deadline_open,
            i.latest,
        )
    });
}

#[test]
fn delivery_resolution_matches_lean() {
    gents_lean_vocab::families::session_operations_resolve_delivery::replay(|i| {
        i.choice.resolve()
    });
}
