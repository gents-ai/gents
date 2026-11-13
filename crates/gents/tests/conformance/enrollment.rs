use gents_lean_vocab::lean_contract_snapshot;

#[test]
fn generated_causal_hop_cases_match_native_materializer() {
    let contract = lean_contract_snapshot();
    assert_eq!(contract.causal_hop_default_max_request_hop, None);
}
