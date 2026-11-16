use super::*;
use gents_lean_vocab::families::{
    client_delegation_outbox_acknowledgement as acknowledgement,
    client_delegation_outbox_preparation as preparation,
    client_delegation_outbox_reservation as reservation,
    client_delegation_outbox_selection as selection,
};

fn token(reservation: Reservation) -> String {
    serde_json::to_value(reservation)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn authored_outbox_transitions_follow_the_model() {
    for case in reservation::cases() {
        let input = &case.input;
        case.check(token(reserve_decision(
            input.local_signer,
            input.binding_valid,
            input.retained_exists,
            input.same,
        )));
    }
    for case in preparation::cases() {
        let input = &case.input;
        case.check(token(prepare_decision(
            input.binding_matches,
            input.pending,
            input.retained_exists,
            input.same,
        )));
    }
    for case in acknowledgement::cases() {
        let input = &case.input;
        case.check(acknowledges(input.binding_matches, input.runtime_confirmed));
    }
    for case in selection::cases() {
        let input = &case.input;
        case.check(select(&input.pending, input.steer.as_deref()).map(str::to_owned));
    }
}
