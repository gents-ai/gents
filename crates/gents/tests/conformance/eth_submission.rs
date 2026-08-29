use crate::lean_vocab_test::assert_state_machine_contract_is_complete;

#[test]
fn eth_submission_has_a_complete_machine_contract() {
    assert_state_machine_contract_is_complete("EthSubmission");
}
